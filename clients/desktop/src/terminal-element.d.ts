import type { HostProps } from "@gpuix/native/host";
import type { SolidHostProps } from "@gpuix/solid/jsx-runtime";

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

/**
 * A request the focused terminal runs natively once per new `id`: `copy` its
 * selection and `paste` the clipboard (the Command-C / Command-V path, even
 * while the find bar holds the keyboard), `copyText` onto the clipboard, or
 * `open` a path with its default app.
 */
export type HostAction =
  | { id: string; kind: "copy" | "paste" }
  | { id: string; kind: "copyText" | "open"; text: string };

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
          /** One platform request per new id; see HostAction. */
          hostAction?: HostAction | undefined;
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
