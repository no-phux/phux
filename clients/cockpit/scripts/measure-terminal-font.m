// Headless real-host check for the Geist terminal choice. Build with the
// clang flags and NATIVE_SDK_APPKIT_HOST define in host-raster-check.sh,
// replacing its source argument with scripts/measure-terminal-font.m; run
// from the Cockpit source root (or in a bundle carrying assets/fonts).
// Reuse the existing host translation unit and its required linker stubs.
// No window, app event loop, font installation or user config is involved.
#define main raster_measure_main
#include "measure-host-raster.m"
#undef main

int main(void) {
    @autoreleasepool {
        native_sdk_appkit_register_bundled_fonts();
        NSFont *font = NativeSdkBuiltInFontForFontId(2, 13);
        fprintf(stdout, "builtin terminal font 2: %s\n", font.fontName.UTF8String);
        return [font.fontName isEqualToString:@"GeistMono-Regular"] ? 0 : 1;
    }
}
