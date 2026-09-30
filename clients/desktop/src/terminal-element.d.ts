import type { HostProps } from "@gpuix/native/host";
import type { SolidHostProps } from "@gpuix/solid/jsx-runtime";

declare module "@gpuix/native/host" {
  export function registerCustomElementType(
    name: string,
    props: HostProps | SolidHostProps
  ): void;
}

export {};
