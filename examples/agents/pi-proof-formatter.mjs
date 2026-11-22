#!/usr/bin/env node
import readline from "node:readline";

const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  if (!line.trim()) continue;
  let event;
  try {
    event = JSON.parse(line);
  } catch {
    continue;
  }
  if (event.type === "tool_execution_start") {
    process.stdout.write(`\n→ ${event.toolName} ${JSON.stringify(event.args)}\n`);
  } else if (event.type === "tool_execution_end") {
    process.stdout.write(`${event.isError ? "✗" : "✓"} ${event.toolName}\n`);
    for (const item of event.result?.content ?? []) {
      if (item.type === "text" && item.text) process.stdout.write(`${item.text}\n`);
    }
  } else if (event.type === "message_end" && event.message?.role === "assistant") {
    const text = (event.message.content ?? [])
      .filter((item) => item.type === "text")
      .map((item) => item.text)
      .join("");
    if (text) process.stdout.write(`\nPi: ${text}\n`);
  }
}
