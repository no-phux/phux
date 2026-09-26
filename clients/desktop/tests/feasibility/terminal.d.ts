import type { HostProps } from "@gpuix/native/host";
import type { SolidHostProps } from "@gpuix/solid/jsx-runtime";

// The verified source patch exports this. Published 0.10.0 types do not.
declare module "@gpuix/native/host" {
  export function registerCustomElementType(type: string): string;
}

declare module "@gpuix/solid/jsx-runtime" {
  namespace JSX {
    interface IntrinsicElements {
      "phux-terminal": SolidHostProps<
        HostProps & {
          clientHandle: string;
          terminalId: string;
          viewId: string;
          paintRevision: number;
          focused: boolean;
        }
      >;
    }
  }
}
