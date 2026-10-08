// Headless test of the pinned SDK's real CoreText registry and rasterizer.
// Reuse its existing measurement harness for host inclusion and updater stubs.
#define main paper_mono_baseline_main
#include "measure-host-raster.m"
#undef main

int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc != 2) return 2;
        NSString *root = [NSString stringWithUTF8String:argv[1]];
        const uint64_t ids[] = {68, 69, 64};
        NSArray *files = @[@"PaperMono-Regular.ttf", @"PaperMono-Bold.ttf", @"JetBrainsMonoNLNerdFontMono-Regular.ttf"];
        uint64_t tokens[3] = {0};
        for (NSUInteger index = 0; index < 3; index++) {
            NSData *data = [NSData dataWithContentsOfFile:[root stringByAppendingPathComponent:files[index]]];
            if (!data || !native_sdk_appkit_register_font(ids[index], data.bytes, data.length, &tokens[index])) return 1;
        }
        for (NSUInteger index = 0; index < 2; index++) {
            NSFont *font = NativeSdkFontForFontId(ids[index], 14);
            if (![font.familyName isEqualToString:@"Paper Mono"]) {
                fprintf(stderr, "wrong host face at %llu: %s\n", ids[index], font.familyName.UTF8String);
                return 1;
            }
            printf("host id=%llu family=%s face=%s\n", ids[index], font.familyName.UTF8String, font.fontName.UTF8String);
            UniChar chars[] = {'M', 'i'};
            CGGlyph glyphs[2];
            CGSize advances[2];
            if (!CTFontGetGlyphsForCharacters((__bridge CTFontRef)font, chars, glyphs, 2)) return 1;
            CTFontGetAdvancesForGlyphs((__bridge CTFontRef)font, kCTFontOrientationHorizontal, glyphs, advances, 2);
            if (fabs(advances[0].width - advances[1].width) > 0.001) return 1;
            printf("host mono advance=%.5f\n", advances[0].width);
        }
        NativeSdkCellFontIds paper = {68, 69, 0, 0};
        NativeSdkCellFace italic = NativeSdkCellFaceFor(paper, NativeSdkCellFlagItalic);
        NativeSdkCellFace boldItalic = NativeSdkCellFaceFor(paper, NativeSdkCellFlagBold | NativeSdkCellFlagItalic);
        if (italic.fontId != 68 || !italic.syntheticItalic || italic.syntheticBold) return 1;
        if (boldItalic.fontId != 69 || !boldItalic.syntheticItalic || boldItalic.syntheticBold) return 1;
        // Supplementary Nerd codepoint F0206, alongside BMP Powerline E0B0.
        NSFont *nerd = NativeSdkFontForFontId(64, 14);
        NSString *symbols = @"\uE0B0\U000F0206";
        CTLineRef symbolLine = CTLineCreateWithAttributedString((__bridge CFAttributedStringRef)
            [[NSAttributedString alloc] initWithString:symbols attributes:@{NSFontAttributeName: nerd}]);
        for (id item in (__bridge NSArray *)CTLineGetGlyphRuns(symbolLine)) {
            CTRunRef run = (__bridge CTRunRef)item;
            CFIndex count = CTRunGetGlyphCount(run);
            CGGlyph glyphs[count];
            CTRunGetGlyphs(run, CFRangeMake(0, 0), glyphs);
            for (CFIndex index = 0; index < count; index++) if (!glyphs[index]) return 1;
        }
        CFRelease(symbolLine);
        NativeSdkMetalSurfaceView *view = [[NativeSdkMetalSurfaceView alloc] initWithFrame:NSMakeRect(0, 0, 640, 64)];
        [view stopDisplayTimer];
        if (!view.canvasColorSpace) view.canvasColorSpace = CGColorSpaceCreateDeviceRGB();
        for (NSUInteger style = 0; style < 4; style++) {
            NSMutableDictionary *command = [CellGridCommand(kSampleText.length) mutableCopy];
            NSMutableDictionary *grid = [command[@"cellGrid"] mutableCopy];
            grid[@"font"] = @68; grid[@"boldFont"] = @69;
            NSMutableArray *cells = [NSMutableArray array];
            for (NSDictionary *source in grid[@"cells"]) {
                NSMutableDictionary *cell = [source mutableCopy];
                cell[@"flags"] = @(NativeSdkCellFlagHasBackground | style);
                [cells addObject:cell];
            }
            grid[@"cells"] = cells; command[@"cellGrid"] = grid;
            NativeSdkPacketCommandRaster *raster = [view rasterCacheBuildEntryForCommand:command kind:@"cell_grid"
                scale:kScale pixelWidth:kSampleText.length * kCellWidth * kScale pixelHeight:kCellHeight * kScale];
            Ink ink;
            if (!raster.image || !MeasureImage(raster.image, &ink) || !ink.solid) return 1;
            printf("host Paper style=%lu solid=%llu lit=%llu\n", (unsigned long)style, ink.solid, ink.lit);
        }
        for (NSUInteger index = 0; index < 3; index++) native_sdk_appkit_unregister_font(ids[index], tokens[index]);
        puts("ok: pinned host Paper registration, metrics, all SGR faces and Nerd fallback");
        return 0;
    }
}
