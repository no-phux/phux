import type { HostProps } from "@gpuix/native/host";
import type { SolidHostProps } from "@gpuix/solid/jsx-runtime";

declare global {
  var phuxShortcut: ((chord: string) => void) | undefined;
  var phuxOpenWindow: ((placement: MovedView) => void) | undefined;
}

/** A runtime view handed to a window of its own. */
export interface MovedView {
  clientHandle: string;
  terminalId: string;
  viewId: string;
  title: string;
  font?: {
    family: string;
    size: number;
    lineHeight: number;
    cellWidth?: number;
    cellHeight?: number;
  };
  theme?: TerminalTheme;
}

declare module "@gpuix/native/host" {
  export function registerCustomElementType(type: string): string;
}

export interface TerminalTheme {
  foreground: string;
  background: string;
  cursor: string;
  selectionForeground: string;
  selectionBackground: string;
  /** Exactly 16 ANSI colours; absent keeps the terminal's own palette. */
  palette?: string[];
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
          sizeOwner?: boolean;
          optionAsAlt?: boolean;
          appChords?: string[];
          font?: {
            family: string;
            size: number;
            lineHeight: number;
            cellWidth?: number;
            cellHeight?: number;
          };
          theme?: TerminalTheme;
        }
      >;
      /** Empty chrome that moves the window, and zooms it on double-click. */
      "phux-drag-region": SolidHostProps<HostProps>;
    }
  }
}
