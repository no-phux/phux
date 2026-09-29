import { createRoot, type JSX } from "@gpuix/solid";
import type { EventPayload, GpuixRenderer } from "../native/generated/index";
import type { MovedView } from "./terminal-element";

/**
 * A placement moved into its own window. It keeps the same runtime view, so
 * the process, scrollback position and selection carry over untouched.
 */
export function openOtherWindow(
  Renderer: new (callback: (error: Error | null, event: EventPayload) => void) => GpuixRenderer,
  placement: MovedView,
): void {
  let root: ReturnType<typeof createRoot> | undefined;
  const renderer = new Renderer((error, event) => {
    if (!error) root?.dispatch(event);
  });
  renderer.init({ title: placement.title, width: 820, height: 540, focus: false });
  root = createRoot(renderer);
  root.render((): JSX.Element => <MovedTerminal placement={placement} />);
}

function MovedTerminal(props: { placement: MovedView }): JSX.Element {
  return (
    <div
      style={{
        display: "flex",
        width: "100%",
        height: "100%",
        paddingTop: 6,
        paddingLeft: 8,
        paddingRight: 4,
        backgroundColor: props.placement.theme?.background ?? "#000000",
      }}
    >
      <phux-terminal
        clientHandle={props.placement.clientHandle}
        terminalId={props.placement.terminalId}
        viewId={props.placement.viewId}
        paintRevision={1}
        focused={true}
        {...(props.placement.font ? { font: props.placement.font } : {})}
        {...(props.placement.theme ? { theme: props.placement.theme } : {})}
        style={{ flexGrow: 1, minWidth: 0, minHeight: 0 }}
      />
    </div>
  );
}
