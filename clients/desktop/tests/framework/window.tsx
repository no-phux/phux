import { createSignal } from "solid-js";
import { render, type JSX } from "@gpuix/solid";

function FrameworkWindow(): JSX.Element {
  const [count, setCount] = createSignal(0);
  const [text, setText] = createSignal("");
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 16,
        padding: 24,
        height: "100%",
        backgroundColor: "#15171c",
      }}
    >
      <text style={{ color: "#eeeeee" }}>phux native framework verification</text>
      <text style={{ color: "#9da8b7" }}>Solid state, native events and production JSX</text>
      <div
        testId="increment"
        onClick={() => setCount((value) => value + 1)}
        style={{ padding: 12, backgroundColor: "#28374d", cursor: "pointer" }}
      >
        <text style={{ color: "#ffffff" }}>{`Count: ${count()}`}</text>
      </div>
      <input
        testId="entry"
        value={text()}
        onChange={(event) => setText(event.value ?? "")}
        style={{ height: 40, color: "#ffffff", backgroundColor: "#28374d" }}
      />
      <text style={{ color: "#ffffff" }}>{`Typed: ${text()}`}</text>
    </div>
  );
}

render((): JSX.Element => <FrameworkWindow />, {
  title: "phux framework verification",
  width: 640,
  height: 360,
  focus: false,
});
