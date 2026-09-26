import type { HostProps } from "@gpuix/native/host";
import type { SolidHostProps } from "@gpuix/solid/jsx-runtime";

declare global {
  var phuxOpenWindow:
    | ((placement: {
        clientHandle: string;
        terminalId: string;
        viewId: string;
        title: string;
      }) => void)
    | undefined;
}

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
          optionAsAlt?: boolean;
          font?: { family: string; size: number; lineHeight: number };
        }
      >;
    }
  }
}
