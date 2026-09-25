import { createRoot, type JSX } from "@gpuix/solid";
import type { EventPayload, GpuixRenderer } from "../native/generated/index";

interface MovedView {
  clientHandle: string;
  terminalId: string;
  viewId: string;
  title: string;
}

export function openOtherWindow(
  Renderer: new (
    callback: (error: Error | null, event: EventPayload) => void,
  ) => GpuixRenderer,
  placement: MovedView,
): void {
  let root: ReturnType<typeof createRoot> | undefined;
  const renderer = new Renderer((error, event) => {
    if (!error) root?.dispatch(event);
  });
  renderer.init({ title: placement.title, width: 720, height: 480, focus: false });
  root = createRoot(renderer);
  root.render((): JSX.Element => <MovedTerminal placement={placement} />);
}

function MovedTerminal(props: { placement: MovedView }): JSX.Element {
  return (
    <phux-terminal
      clientHandle={props.placement.clientHandle}
      terminalId={props.placement.terminalId}
      viewId={props.placement.viewId}
      paintRevision={1}
      focused={true}
      style={{ width: "100%", height: "100%" }}
    />
  );
}
