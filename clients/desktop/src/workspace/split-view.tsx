import { Match, Switch, type JSX, type Accessor } from "solid-js";
import type { StyleDesc } from "@gpuix/solid";
import { usePalette } from "../ui/controls";
import type { Axis, LayoutNode, Placement } from "./layout";

export const DIVIDER = 5;

interface SplitProps {
  pane: (placement: Placement) => JSX.Element;
  beginResize: (splitId: string, axis: Axis, container: number) => void;
  dragging: string | undefined;
}

type SplitNode = Extract<LayoutNode, { kind: "split" }>;

/**
 * Recursive split renderer. Ratios become flex-grow weights, so GPUI does the
 * arithmetic; a divider reports its split and the container element whose
 * bounds turn pointer position back into a ratio. Leaves are keyed by their
 * placement object, which layout operations preserve, so a terminal element
 * is rebuilt only when a different placement occupies the slot.
 */
export function SplitView(props: SplitProps & { node: LayoutNode }): JSX.Element {
  return (
    <Switch>
      <Match when={props.node.kind === "leaf" ? props.node.placement : undefined} keyed>
        {(placement: Placement): JSX.Element => props.pane(placement)}
      </Match>
      <Match when={props.node.kind === "split" ? props.node : undefined}>
        {(split: Accessor<SplitNode>): JSX.Element => (
          <Split
            split={split()}
            pane={props.pane}
            beginResize={props.beginResize}
            dragging={props.dragging}
          />
        )}
      </Match>
    </Switch>
  );
}

function Split(props: SplitProps & { split: SplitNode }): JSX.Element {
  const colors = usePalette();
  let container = 0;
  const horizontal = (): boolean => props.split.axis === "row";
  const active = (): boolean => props.dragging === props.split.id;
  const thickness = (size: number): StyleDesc =>
    horizontal() ? { width: size, alignSelf: "stretch" } : { height: size, alignSelf: "stretch" };
  return (
    <div
      ref={(element: { id: number }): JSX.Element => {
        container = element.id;
      }}
      style={{
        display: "flex",
        flexDirection: horizontal() ? "row" : "column",
        flexGrow: 1,
        flexBasis: 0,
        minWidth: 0,
        minHeight: 0,
      }}
    >
      <div style={cell(props.split.ratio)}>
        <SplitView
          node={props.split.first}
          pane={props.pane}
          beginResize={props.beginResize}
          dragging={props.dragging}
        />
      </div>
      <div
        role="separator"
        aria-label="Resize split"
        onMouseDown={() => props.beginResize(props.split.id, props.split.axis, container)}
        style={{
          ...thickness(DIVIDER),
          flexShrink: 0,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          flexDirection: horizontal() ? "row" : "column",
          cursor: horizontal() ? "col-resize" : "row-resize",
          backgroundColor: active() ? `${colors().accent}33` : "transparent",
          hover: { backgroundColor: `${colors().accent}22` },
        }}
      >
        <div
          style={{
            ...thickness(1),
            backgroundColor: active() ? colors().accent : colors().border,
          }}
        />
      </div>
      <div style={cell(1 - props.split.ratio)}>
        <SplitView
          node={props.split.second}
          pane={props.pane}
          beginResize={props.beginResize}
          dragging={props.dragging}
        />
      </div>
    </div>
  );
}

function cell(grow: number): StyleDesc {
  return { display: "flex", flexGrow: grow, flexBasis: 0, minWidth: 0, minHeight: 0 };
}
