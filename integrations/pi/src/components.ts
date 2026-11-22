import type { Theme } from "@earendil-works/pi-coding-agent";
import {
  SelectList,
  truncateToWidth,
  type Component,
  type SelectItem,
} from "@earendil-works/pi-tui";

import type { AgentPane } from "./schemas.js";
import { formatPaneDisplay } from "./target-store.js";

export class PhuxTargetPicker implements Component {
  private readonly list: SelectList;

  constructor(
    panes: readonly AgentPane[],
    private readonly theme: Theme,
    onSelect: (pane: AgentPane) => void,
    onCancel: () => void,
    private readonly requestRender: () => void = () => {},
  ) {
    const bySelector = new Map(panes.map((pane) => [pane.terminal, pane]));
    const items: SelectItem[] = panes.map((pane) => ({
      value: pane.terminal,
      label: formatPaneDisplay(pane),
      description: `${pane.state}, attention ${pane.attention}`,
    }));
    this.list = new SelectList(items, Math.min(Math.max(items.length, 1), 10), {
      selectedPrefix: (text) => theme.fg("accent", text),
      selectedText: (text) => theme.fg("accent", text),
      description: (text) => theme.fg("muted", text),
      scrollInfo: (text) => theme.fg("dim", text),
      noMatch: (text) => theme.fg("warning", text),
    });
    this.list.onSelect = (item) => {
      const pane = bySelector.get(item.value);
      if (pane !== undefined) onSelect(pane);
    };
    this.list.onCancel = onCancel;
  }

  render(width: number): string[] {
    if (width <= 0) return [];
    const title = this.theme.fg("accent", this.theme.bold("Select a phux target"));
    const help = this.theme.fg("dim", "up/down navigate, enter select, esc cancel");
    return [title, ...this.list.render(width), help]
      .map((line) => truncateToWidth(line, width));
  }

  invalidate(): void {
    // The theme object is live; avoid cached styled strings and invalidate the child.
    this.list.invalidate();
  }

  handleInput(data: string): void {
    this.list.handleInput(data);
    this.requestRender();
  }
}
