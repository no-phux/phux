// Rasterize a terminal row through the SDK host's OWN rasterizer, headlessly,
// and measure the ink.
//
// This is not a re-implementation and not a replica. It #includes the pinned
// SDK's `src/platform/macos/appkit_host.m` - the exact translation unit the
// shipping app links - so the CGBitmapContext configuration, the CoreText
// draw, the face resolution and the two-pass cell order are the host's, not
// this file's. If a future SDK bump turns macOS font smoothing off again, or
// changes the colour space, or changes how a cell inks, the numbers below move
// without this file being touched.
//
// Build and run through scripts/host-raster-check.sh, which resolves the
// pinned SDK and passes the right flags. Directly:
//
//   clang -w -fobjc-arc -fno-sanitize=builtin -ObjC -mmacosx-version-min=11.0 \
//       -DNATIVE_SDK_APPKIT_HOST='"<sdk>/src/platform/macos/appkit_host.m"' \
//       -o /tmp/measure-host-raster scripts/measure-host-raster.m \
//       -framework Foundation -framework AppKit -framework Metal \
//       -framework QuartzCore -framework CoreText -framework CoreGraphics \
//       -framework ImageIO -framework AVFoundation \
//       -framework UniformTypeIdentifiers -framework WebKit \
//       -framework Security -framework ScreenCaptureKit -framework CoreMedia \
//       -framework CoreVideo -framework IOKit -framework Carbon \
//       -framework Accelerate -framework MediaToolbox
//   /tmp/measure-host-raster src/fonts/JetBrainsMonoNLNerdFontMono-Regular.ttf
//
// WHY IT EXISTS (phux-cockpit-2ml.3)
// ----------------------------------
// `native automate screenshot` renders through the SDK's deterministic CPU
// reference renderer, which fills glyph outlines with its own std-only
// TrueType rasterizer and never calls CoreText. Every defect that lives in
// the real rasterizer is therefore invisible to every automated screenshot,
// by construction. phux-cockpit-aht — all terminal text ~35% too thin — is
// the worked example, but its first smoothing diagnosis was wrong: the
// shipped "before" commit omitted the smoothing calls while this bitmap
// context already defaulted to smoothing enabled.
//
// THE BASIS
// ---------
// The row is inked over an OPAQUE terminal background (the cells carry
// `has_background` and #090b0f), so the raster is opaque and luminance is a
// direct read of glyph coverage: luma = 0.2126R + 0.7152G + 0.0722B on the
// image's own bytes, no colour conversion. `solid` counts luma > 127 and
// `lit` counts luma > 32 — the same two thresholds scripts/measure-png-ink.m
// reports.
#import <Foundation/Foundation.h>
#import <AppKit/AppKit.h>

#ifndef NATIVE_SDK_APPKIT_HOST
#error "define NATIVE_SDK_APPKIT_HOST to the pinned SDK's appkit_host.m path"
#endif

#include NATIVE_SDK_APPKIT_HOST

// The raster builder is defined in the host's @implementation but not declared
// in its @interface. Declaring it here is what lets this harness call the real
// thing instead of copying it.
@interface NativeSdkMetalSurfaceView (PhuxCockpitHostRaster)
- (NativeSdkPacketCommandRaster *)rasterCacheBuildEntryForCommand:(NSDictionary *)command
                                                             kind:(NSString *)kind
                                                            scale:(CGFloat)scale
                                                       pixelWidth:(NSUInteger)pixelWidth
                                                      pixelHeight:(NSUInteger)pixelHeight;
@end

// The row under test. Fixed on purpose: the numbers this prints are pinned in
// docs/RENDER_FIDELITY.md and asserted by CI, so the content may not drift.
static NSString *const kSampleText = @"the quick brown fox jumps over the lazy dog $ ls";

static const CGFloat kFontSize = 13.0;
static const CGFloat kCellWidth = 8.0;
static const CGFloat kCellHeight = 18.0;
static const CGFloat kBaseline = 14.0;
static const CGFloat kScale = 2.0;

// #f4f7fb on #090b0f, the app's terminal foreground on its terminal ground.
static NSArray *ForegroundColor(void) { return @[ @(0.957), @(0.969), @(0.984), @(1.0) ]; }
static NSArray *BackgroundColor(void) { return @[ @(0.035), @(0.043), @(0.059), @(1.0) ]; }

static NSDictionary *CellGridCommand(NSUInteger cols) {
    NSMutableArray *cells = [NSMutableArray arrayWithCapacity:cols];
    for (NSUInteger column = 0; column < cols; column++) {
        NSMutableDictionary *cell = [NSMutableDictionary dictionary];
        cell[@"fg"] = ForegroundColor();
        cell[@"bg"] = BackgroundColor();
        cell[@"ul"] = ForegroundColor();
        cell[@"flags"] = @(NativeSdkCellFlagHasBackground);
        if (column < kSampleText.length) {
            cell[@"text"] = [kSampleText substringWithRange:NSMakeRange(column, 1)];
        }
        [cells addObject:cell];
    }
    return @{
        @"kind" : @"cell_grid",
        @"bounds" : @[ @(0), @(0), @(cols * kCellWidth), @(kCellHeight) ],
        @"opacity" : @(1.0),
        @"cellGrid" : @{
            @"font" : @(1),
            @"boldFont" : @(0),
            @"italicFont" : @(0),
            @"boldItalicFont" : @(0),
            @"size" : @(kFontSize),
            @"origin" : @[ @(0), @(0) ],
            @"cellWidth" : @(kCellWidth),
            @"cellHeight" : @(kCellHeight),
            @"baseline" : @(kBaseline),
            @"cols" : @(cols),
            @"rows" : @(1),
            @"cells" : cells,
        },
    };
}

// A plain text run over the same ground, drawn by the host's `draw_text` arm.
// Terminal cells and chrome labels take different draw calls into the SAME
// bitmap, so measuring both says whether a defect is in the context setup or
// in one draw arm.
static NSDictionary *DrawTextCommand(NSUInteger cols) {
    return @{
        @"kind" : @"draw_text",
        @"bounds" : @[ @(0), @(0), @(cols * kCellWidth), @(kCellHeight) ],
        @"opacity" : @(1.0),
        @"text" : @{
            @"text" : kSampleText,
            @"color" : ForegroundColor(),
            @"size" : @(kFontSize),
            @"origin" : @[ @(0), @(kBaseline) ],
            @"font" : @(1),
        },
    };
}

typedef struct {
    size_t width;
    size_t height;
    double mean;
    unsigned long long solid;
    unsigned long long lit;
} Ink;

static BOOL MeasureImage(CGImageRef image, Ink *out) {
    if (!image) return NO;
    if (CGImageGetBitsPerComponent(image) != 8 || CGImageGetBitsPerPixel(image) != 32) return NO;
    CGDataProviderRef provider = CGImageGetDataProvider(image);
    CFDataRef data = provider ? CGDataProviderCopyData(provider) : NULL;
    if (!data) return NO;
    const uint8_t *px = CFDataGetBytePtr(data);
    const size_t width = CGImageGetWidth(image);
    const size_t height = CGImageGetHeight(image);
    const size_t stride = CGImageGetBytesPerRow(image);
    double sum = 0;
    unsigned long long solid = 0, lit = 0;
    for (size_t y = 0; y < height; y++) {
        const uint8_t *row = px + y * stride;
        for (size_t x = 0; x < width; x++) {
            const uint8_t *p = row + x * 4;
            const double luma = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
            sum += luma;
            if (luma > 127.0) solid++;
            if (luma > 32.0) lit++;
        }
    }
    CFRelease(data);
    out->width = width;
    out->height = height;
    out->mean = (width * height) > 0 ? sum / (double)(width * height) : 0.0;
    out->solid = solid;
    out->lit = lit;
    return YES;
}

static void WritePng(CGImageRef image, NSString *path) {
    if (!image || path.length == 0) return;
    NSURL *url = [NSURL fileURLWithPath:path];
    CGImageDestinationRef destination =
        CGImageDestinationCreateWithURL((__bridge CFURLRef)url, CFSTR("public.png"), 1, NULL);
    if (!destination) return;
    CGImageDestinationAddImage(destination, image, NULL);
    CGImageDestinationFinalize(destination);
    CFRelease(destination);
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc < 2) {
            fprintf(stderr, "usage: measure-host-raster <font.ttf> [--png-prefix PREFIX] [--min-solid N]\n");
            return 2;
        }
        NSString *fontPath = [NSString stringWithUTF8String:argv[1]];
        NSString *pngPrefix = nil;
        long long minSolid = -1;
        for (int index = 2; index + 1 < argc; index += 2) {
            if (strcmp(argv[index], "--png-prefix") == 0) pngPrefix = [NSString stringWithUTF8String:argv[index + 1]];
            else if (strcmp(argv[index], "--min-solid") == 0) minSolid = atoll(argv[index + 1]);
        }

        // Register the app's own face under font id 1, through the host's own
        // registration entry point, so the raster resolves the same face the
        // app ships rather than a system fallback.
        NSData *fontData = [NSData dataWithContentsOfFile:fontPath];
        if (!fontData) {
            fprintf(stderr, "cannot read font %s\n", argv[1]);
            return 1;
        }
        uint64_t token = 0;
        if (native_sdk_appkit_register_font(1, fontData.bytes, fontData.length, &token) != 1) {
            fprintf(stderr, "the host refused to register %s\n", argv[1]);
            return 1;
        }

        // A surface view is the object the raster builder hangs off. It needs
        // no window and never presents here; the Metal device it creates is
        // untouched by the raster path.
        NativeSdkMetalSurfaceView *view =
            [[NativeSdkMetalSurfaceView alloc] initWithFrame:NSMakeRect(0, 0, 640, 64)];
        if (!view) {
            fprintf(stderr, "could not create the host surface view\n");
            return 1;
        }
        [view stopDisplayTimer];
        // The same colour space the host installs for every canvas raster.
        if (!view.canvasColorSpace) view.canvasColorSpace = CGColorSpaceCreateDeviceRGB();

        const NSUInteger cols = kSampleText.length;
        const NSUInteger pixelWidth = (NSUInteger)(cols * kCellWidth * kScale);
        const NSUInteger pixelHeight = (NSUInteger)(kCellHeight * kScale);

        struct {
            const char *name;
            NSDictionary *command;
            NSString *kind;
        } cases[] = {
            {"cell_grid", CellGridCommand(cols), @"cell_grid"},
            {"draw_text", DrawTextCommand(cols), @"draw_text"},
        };

        printf("sdk=%s\n", NATIVE_SDK_APPKIT_HOST);
        printf("font=%s size=%.1f scale=%.1f cols=%lu\n",
               argv[1], (double)kFontSize, (double)kScale, (unsigned long)cols);

        long long cellGridSolid = -1;
        for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
            NativeSdkPacketCommandRaster *entry =
                [view rasterCacheBuildEntryForCommand:cases[index].command
                                                 kind:cases[index].kind
                                                scale:kScale
                                           pixelWidth:pixelWidth
                                          pixelHeight:pixelHeight];
            if (!entry || !entry.image) {
                fprintf(stderr, "the host rasterizer returned nothing for %s\n", cases[index].name);
                return 1;
            }
            Ink ink;
            if (!MeasureImage(entry.image, &ink)) {
                fprintf(stderr, "unexpected raster layout for %s\n", cases[index].name);
                return 1;
            }
            printf("kind=%-9s width=%zu height=%zu mean_luma=%.4f solid=%llu lit=%llu\n",
                   cases[index].name, ink.width, ink.height, ink.mean, ink.solid, ink.lit);
            if (strcmp(cases[index].name, "cell_grid") == 0) cellGridSolid = (long long)ink.solid;
            if (pngPrefix) {
                WritePng(entry.image, [NSString stringWithFormat:@"%@-%s.png", pngPrefix, cases[index].name]);
            }
        }

        if (minSolid >= 0) {
            if (cellGridSolid < minSolid) {
                fprintf(stderr,
                        "FAIL: cell_grid solid=%lld is below the pinned floor %lld.\n"
                        "      The host rasterizer got thinner. No reference screenshot can show this.\n",
                        cellGridSolid, minSolid);
                return 1;
            }
            printf("ok: cell_grid solid=%lld >= %lld\n", cellGridSolid, minSolid);
        }
        return 0;
    }
}
