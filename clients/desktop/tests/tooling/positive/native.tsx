import { createSignal, mergeProps } from "solid-js";
import type { EventPayload, JSX } from "@gpuix/solid";

export function NativeFixture(props: { title: string }): JSX.Element {
  const merged = mergeProps({ title: "Default" }, props);
  const [clicks, setClicks] = createSignal(0);
  const click = (_event: EventPayload) => setClicks((count) => count + 1);
  return (
    <div onClick={click}>
      <text>
        {merged.title}: {clicks()}
      </text>
      <input value={props.title} onChange={(event) => setClicks(event.eventType.length)} />
      <anchored>
        <text>Native host</text>
      </anchored>
    </div>
  );
}

export function parseTitle(value: unknown): string {
  if (typeof value !== "object" || value === null || !("title" in value)) {
    throw new Error("Expected an object with a title");
  }
  if (typeof value.title !== "string") throw new Error("Expected a string title");
  return value.title;
}

export async function parseFile(file: Bun.BunFile): Promise<string> {
  const value: unknown = await file.json();
  return parseTitle(value);
}
