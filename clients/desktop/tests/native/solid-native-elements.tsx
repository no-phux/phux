import assert from "node:assert/strict";
import { resolve } from "node:path";
import { createSignal, Show } from "solid-js";
import { createRoot, GpuixRenderer, type JSX } from "@gpuix/solid";
import { methods } from "@gpuix/native/automation";
import { registerCustomElementType, type HostProps } from "@gpuix/native/host";
import type { SolidHostProps } from "@gpuix/solid/jsx-runtime";

declare module "@gpuix/solid/jsx-runtime" {
  namespace JSX {
    interface IntrinsicElements {
      "phux-host-probe": SolidHostProps<HostProps & { label: string }>;
      "missing-native-probe": SolidHostProps;
    }
  }
}

declare global {
  var solidElementsHost: {
    GpuixRenderer: typeof GpuixRenderer;
    desktopHostProbeCounts(): {
      created: number;
      destroyed: number;
      dropped: number;
      painted: number;
    };
  };
}

const host = globalThis.solidElementsHost;
assert.equal(host.GpuixRenderer, GpuixRenderer);
assert.equal(registerCustomElementType("phux-host-probe"), "phux-host-probe");
registerCustomElementType("missing-native-probe");
const errors: unknown[] = [];
let root: ReturnType<typeof createRoot>;
let clicks = 0;
const renderer = new GpuixRenderer((error, event) => {
  if (error) errors.push(error);
  else root?.dispatch(event);
});
assert.throws(() => renderer.hasCustomElementType("phux-host-probe"), /not initialized/);
renderer.init({ title: "Solid native extension", width: 380, height: 180, focus: false });
assert.equal(renderer.hasCustomElementType("phux-host-probe"), true);
assert.equal(renderer.hasCustomElementType("missing-native-probe"), false);
assert.equal(host.desktopHostProbeCounts().created, 0, "factory queries never create instances");
root = createRoot(renderer, { onUncaughtError: (error) => errors.push(error) });
const [label, setLabel] = createSignal("Solid native first");
const [shown, setShown] = createSignal(true);

async function pump() {
  for (let i = 0; i < 12; i++) {
    assert.equal(renderer.tick(), true);
    await new Promise((resolve) => setTimeout(resolve, 12));
  }
  assert.deepEqual(errors, []);
}

try {
  root.render((): JSX.Element => (
    <div style={{ width: "100%", height: "100%", padding: 16, backgroundColor: "#182030" }}>
      <Show when={shown()}>
        <phux-host-probe
          testId="probe"
          label={label()}
          onClick={() => clicks++}
          style={{ width: 300, height: 80, color: "#ffffff", backgroundColor: "#305080" }}
        />
      </Show>
    </div>
  ));
  await pump();
  assert.deepEqual(renderer.getPaintedText(), ["Solid native first"]);
  const raw: unknown = JSON.parse(renderer.getAutomationTree());
  const tree = methods.getTree.result.parse({ tree: raw }).tree;
  const probe = tree?.children?.[0];
  assert.ok(probe);
  assert.equal(probe.type, "phux-host-probe");
  assert.equal(probe.testId, "probe");
  const bounds = renderer.getElementBounds(probe.id);
  assert.ok(bounds);
  assert.equal(bounds.width, 300);
  assert.equal(bounds.height, 80);
  renderer.simulateClick(bounds.x + 8, bounds.y + 8);
  await pump();
  assert.equal(clicks, 1);
  root.flushSync(() => setLabel("Solid native reactive"));
  await pump();
  assert.deepEqual(renderer.getPaintedText(), ["Solid native reactive"]);
  assert.equal(
    host.desktopHostProbeCounts().created,
    1,
    "reactive props retain the native instance",
  );
  root.flushSync(() => setShown(false));
  await pump();
  assert.deepEqual(renderer.getPaintedText(), []);
  assert.equal(host.desktopHostProbeCounts().destroyed, 1);
  assert.equal(host.desktopHostProbeCounts().dropped, 1);
  root.flushSync(() => setShown(true));
  await pump();
  assert.deepEqual(renderer.getPaintedText(), ["Solid native reactive"]);
  assert.equal(host.desktopHostProbeCounts().created, 2);
  renderer.captureScreenshot(
    resolve(process.env.SOLID_ELEMENTS_ARTIFACTS!, "solid-native-elements.png"),
  );
  root.unmount();
  await pump();
  assert.equal(host.desktopHostProbeCounts().destroyed, 2);
  assert.equal(host.desktopHostProbeCounts().dropped, 2);
  root = createRoot(renderer);
  assert.throws(
    () => root.render((): JSX.Element => <missing-native-probe />),
    /No native factory.*missing-native-probe/,
  );
  assert.equal(host.desktopHostProbeCounts().created, 2);
} finally {
  root.unmount();
  renderer.closeWindow();
}
assert.equal(renderer.tick(), false);
console.log("SOLID_NATIVE_ELEMENTS_PASS", host.desktopHostProbeCounts());
process.exit(0);
