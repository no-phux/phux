import type { AssistantMessage } from "@earendil-works/pi-ai";
import type { MessageChange } from "@earendil-works/pi-durable";

/** Fold one commit's changes into the in-flight assistant message; the kind MessageTranscript keys on. */
export function applyChanges(message: AssistantMessage, changes: readonly MessageChange[]): string | undefined {
  let kind: string | undefined;
  const content = message.content as AssistantMessage["content"][number][];
  for (const change of changes) {
    switch (change.type) {
      case "message":
        Object.assign(message, structuredClone(change.message));
        kind = "text_delta";
        break;
      case "text_start":
      case "thinking_start":
      case "toolcall_start":
      case "block":
        content[change.contentIndex] = structuredClone(change.block);
        if (change.type === "block" && change.block.type === "thinking") kind ??= "thinking_end";
        if (change.type === "block" && change.block.type === "text") kind = "text_delta";
        break;
      case "text_delta": {
        const block = content[change.contentIndex];
        if (block?.type === "text") block.text += change.delta;
        kind = "text_delta";
        break;
      }
      case "thinking_delta": {
        const block = content[change.contentIndex];
        if (block?.type === "thinking") block.thinking += change.delta;
        break;
      }
      default:
        break;
    }
  }
  return kind;
}
