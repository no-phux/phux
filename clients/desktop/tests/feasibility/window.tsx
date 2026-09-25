import { createSignal, onCleanup, onMount, Show, type Accessor } from "solid-js";
import { registerCustomElementType } from "@gpuix/native/host";
import { render, resetRender, type JSX } from "@gpuix/solid";
import type { DesktopClient, DesktopEvent } from "../../native/generated/index";

interface Views {
  terminal: string;
  left: string;
  right: string;
}

interface FixtureProps {
  client: DesktopClient;
  socket: string;
}

const shellCommand =
  "sleep 0.2; for n in $(seq 1 40); do printf 'history %s\\n' \"$n\"; done; printf '\\123\\110\\105\\114\\114 UTF-8: 界 é \u{1F642}\\n'";
const editorText = "EDITOR UTF-8: 界 é \u{1F642}";

function TerminalFixture(props: FixtureProps): JSX.Element {
  const [views, setViews] = createSignal<Views>();
  const [revision, setRevision] = createSignal(0);
  const [status, setStatus] = createSignal("disconnected");
  const [shell, setShell] = createSignal("pending");
  const [editor, setEditor] = createSignal("pending");
  const [vim, setVim] = createSignal("pending");
  const [independent, setIndependent] = createSignal("pending");
  const [metadata, setMetadata] = createSignal("");
  const events: DesktopEvent[] = [];
  let closed = false;

  function accept(batch: DesktopEvent[]): void {
    events.push(...batch);
    for (const event of batch) {
      if (event.kind === "ServerError") setStatus(`failed: ${event.message}`);
    }
  }

  function discoverViews(): void {
    const topology = props.client.topology();
    if (views() || props.client.status() !== "Attached") return;
    const pane = topology?.panes.find((candidate) => candidate.sessionName === "solid-feasibility");
    if (!pane) return;
    if (!props.client.inputReadiness(pane.terminalId).ready) return;
    const left = props.client.createView(pane.terminalId);
    const right = props.client.createView(pane.terminalId);
    if (left === right) throw new Error("Duplicate native view IDs");
    setViews({ terminal: pane.terminalId, left, right });
  }

  function observe(): void {
    const pair = views();
    if (!pair) return;
    const left = props.client.viewInfo(pair.left);
    const right = props.client.viewInfo(pair.right);
    setMetadata(
      `views ${left.viewId}/${right.viewId}; generation ${right.generation}; offsets ${left.scrollOffset}/${right.scrollOffset}`,
    );
    if (props.client.searchView(pair.right, "SHELL UTF-8:", true).length) setShell("ready");
    if (props.client.searchView(pair.right, "VIM READY", true).length) setVim("ready");
    if (props.client.searchView(pair.right, editorText, true).length) setEditor("ready");
  }

  function activity(handle: string): void {
    if (closed) return; // A queued wake after close must not drain a stale handle.
    try {
      updateFromNative(handle);
    } catch (error) {
      shutdown();
      setStatus(`failed: ${String(error)}`);
      console.error(error);
    }
  }

  function updateFromNative(handle: string): void {
    if (handle !== props.client.handle) throw new Error("Unexpected notification owner");
    accept(props.client.takeEvents()); // Exactly one drain, including empty wakes.
    const error = props.client.lastError();
    setStatus(error ? `failed: ${error}` : props.client.status());
    discoverViews();
    observe();
    setRevision((value) => value + 1);
  }

  function inputView(): string {
    const pair = views();
    if (!pair) throw new Error("Input before views exist");
    return pair.right;
  }

  function text(value: string): void {
    if (!props.client.commitText(inputView(), value)) throw new Error("Committed input refused");
  }

  function key(code: number): void {
    if (!props.client.keyEvent(inputView(), { key: code, action: 1, mods: 0, consumedMods: 0 })) {
      throw new Error("Normalized key refused");
    }
  }

  function selectedText(view: string): string {
    try {
      return props.client.viewSelectionText(view);
    } catch (error) {
      // A view with no selection is unavailable, not an empty clipboard string.
      if (String(error).includes("SelectionUnavailable")) return "";
      throw error;
    }
  }

  function command(value: string): void {
    text(value);
    key(58); // PhysicalKey::Enter, declared by phux-protocol.
  }

  function separateViews(): void {
    const pair = views();
    if (!pair) throw new Error("Missing views");
    props.client.scrollView(pair.left, -8);
    const match = props.client.searchView(pair.right, "SHELL UTF-8:", true)[0];
    if (!match) throw new Error("Shell output missing from native replica");
    props.client.setViewSelection(pair.right, match.start, match.end, false);
    if (selectedText(pair.right) !== "SHELL UTF-8:") throw new Error("Selection mismatch");
    if (selectedText(pair.left) !== "") throw new Error("Selection leaked between views");
    if (
      props.client.viewInfo(pair.left).scrollOffset ===
      props.client.viewInfo(pair.right).scrollOffset
    ) {
      throw new Error("Viewport state leaked between views");
    }
    setIndependent("ready");
    observe();
    setRevision((value) => value + 1);
  }

  function startEditor(): void {
    props.client.clearViewSelection(inputView());
    command("vi -u NONE -i NONE -n -c 'set noswapfile' editor.txt");
  }

  function shutdown(): void {
    if (closed) return;
    closed = true;
    const pair = views();
    setViews(undefined);
    if (pair) {
      props.client.destroyView(pair.left);
      props.client.destroyView(pair.right);
    }
    const final = props.client.close();
    accept(final);
    setStatus("closed; views destroyed; final events processed");
    console.error(
      JSON.stringify({
        fixture: "solid-terminal",
        views: pair,
        notifications: revision(),
        events,
        final,
      }),
    );
  }

  onMount(() => {
    const options = {
      socketPath: props.socket,
      cols: 60,
      rows: 16,
      sessionName: "solid-feasibility",
    };
    props.client.connect(options, activity);
  });
  onCleanup(shutdown);

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 10,
        padding: 16,
        height: "100%",
        backgroundColor: "#15171c",
        color: "#eeeeee",
      }}
    >
      <text>Production Solid / native terminal feasibility</text>
      <text>{`Status: ${status()}`}</text>
      <text>{`Shell: ${shell()} | Editor: ${editor()} | Independent: ${independent()}`}</text>
      <text>{`Vim: ${vim()}`}</text>
      <text>{metadata()}</text>
      <div style={{ display: "flex", gap: 12 }}>
        <Action id="shell" label="Shell output" run={() => command(shellCommand)} />
        <Action id="separate" label="Scroll / select" run={separateViews} />
        <Action id="editor" label="Open Vim" run={startEditor} />
        <Action id="insert" label="Insert UTF-8" run={() => text(`cc${editorText}`)} />
        <Action
          id="save"
          label="Save / quit"
          run={() => {
            key(120);
            command(":wq");
          }}
        />
        <Action id="close" label="Close client" run={shutdown} />
      </div>
      <Show when={views()}>
        {(pair: Accessor<Views>): JSX.Element => (
          <div style={{ display: "flex", gap: 12 }}>
            <phux-terminal
              testId="left-terminal"
              clientHandle={props.client.handle}
              terminalId={pair().terminal}
              viewId={pair().left}
              paintRevision={revision()}
              focused={false}
              style={{ width: 520, height: 300 }}
            />
            <phux-terminal
              testId="right-terminal"
              clientHandle={props.client.handle}
              terminalId={pair().terminal}
              viewId={pair().right}
              paintRevision={revision()}
              focused={true}
              style={{ width: 520, height: 300 }}
            />
          </div>
        )}
      </Show>
    </div>
  );
}

function Action(props: { id: string; label: string; run: () => void }): JSX.Element {
  return (
    <div
      testId={props.id}
      onClick={() => props.run()}
      style={{ padding: 10, backgroundColor: "#28374d", cursor: "pointer" }}
    >
      <text>{props.label}</text>
    </div>
  );
}

export function mount(client: DesktopClient, socket: string): void {
  registerCustomElementType("phux-terminal");
  render((): JSX.Element => <TerminalFixture client={client} socket={socket} />, {
    title: "phux Solid terminal feasibility",
    width: 1100,
    height: 500,
    focus: false,
    onUncaughtError: (error) => {
      resetRender();
      throw error;
    },
  });
  process.once("SIGTERM", () => {
    resetRender();
    process.exit(0);
  });
}
