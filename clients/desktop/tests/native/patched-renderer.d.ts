import "@gpuix/native";

// Desktop typecheck uses published 0.10.0 types. These fixtures run against the
// verified source patches, whose declarations are also in native/generated.
declare module "@gpuix/native" {
  interface GpuixRenderer {
    hasCustomElementType(elementType: string): boolean;
    closeWindow(): void;
    isWindowOpen(): boolean;
    getWindowId(): string;
    getWindowTitle(): string;
  }
}

declare module "@gpuix/native/host" {
  export function registerCustomElementType(type: string): string;
}
