import type { AgentPane } from "./schemas.js";

export const PHUX_TARGET_ENTRY = "phux-target";
export const PHUX_TARGET_VERSION = 1 as const;
export const PHUX_NAMED_TARGETS_ENTRY = "phux-named-targets";
export const PHUX_NAMED_TARGETS_VERSION = 1 as const;
export const PHUX_TARGET_NAME_PATTERN = /^[A-Za-z][A-Za-z0-9_-]{0,63}$/;

export interface PhuxTargetSelection {
  readonly version: typeof PHUX_TARGET_VERSION;
  /** Canonical pane selector emitted by phux, such as @3 or host/@3. */
  readonly selector: string;
  /** Owning phux session, used for the human attach handoff. */
  readonly session: string;
  readonly window: string;
  /** Stable human-facing label captured when the target was selected. */
  readonly display: string;
}

export type TargetAvailability = "unselected" | "available" | "stale" | "unavailable";

export interface PhuxTargetSnapshot {
  readonly selection: PhuxTargetSelection | null;
  readonly availability: TargetAvailability;
  readonly reason?: string;
}

export interface PhuxNamedTargetsSnapshot {
  readonly aliases: Readonly<Record<string, PhuxTargetSelection>>;
  readonly groups: Readonly<Record<string, readonly PhuxTargetSelection[]>>;
}

interface PersistedNamedTargets extends PhuxNamedTargetsSnapshot {
  readonly version: typeof PHUX_NAMED_TARGETS_VERSION;
}

export interface BranchEntry {
  readonly type: string;
  readonly customType?: string;
  readonly data?: unknown;
}

export interface TargetInventory {
  agentList(options?: { readonly signal?: AbortSignal }): Promise<{ readonly agents: readonly AgentPane[] }>;
}

export interface TargetPersistence {
  appendEntry<T>(customType: string, data: T): void;
}

export type PhuxTargetListener = (selection: PhuxTargetSelection | null) => void;

export class PhuxTargetStore {
  private snapshotValue: PhuxTargetSnapshot = { selection: null, availability: "unselected" };
  private panesValue: readonly AgentPane[] = [];
  private readonly listeners = new Set<PhuxTargetListener>();
  private publishedSelection: PhuxTargetSelection | null = null;
  private namedValue: PhuxNamedTargetsSnapshot = { aliases: {}, groups: {} };

  constructor(
    private readonly persistence: TargetPersistence,
    private readonly inventory: TargetInventory,
  ) {}

  get snapshot(): PhuxTargetSnapshot {
    return this.snapshotValue;
  }

  get panes(): readonly AgentPane[] {
    return this.panesValue;
  }

  get named(): PhuxNamedTargetsSnapshot {
    return this.namedValue;
  }

  subscribe(listener: PhuxTargetListener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  restoreFromBranch(entries: readonly BranchEntry[]): void {
    let restored: PhuxTargetSelection | null = null;
    let named: PhuxNamedTargetsSnapshot = { aliases: {}, groups: {} };
    let foundSelection = false;
    let foundNamed = false;
    for (let index = entries.length - 1; index >= 0 && (!foundSelection || !foundNamed); index--) {
      const entry = entries[index];
      if (entry?.type !== "custom") continue;
      if (!foundSelection && entry.customType === PHUX_TARGET_ENTRY) {
        restored = parseSelection(entry.data);
        foundSelection = true;
      }
      if (!foundNamed && entry.customType === PHUX_NAMED_TARGETS_ENTRY) {
        named = parseNamedTargets(entry.data);
        foundNamed = true;
      }
    }
    this.namedValue = named;
    this.panesValue = [];
    this.snapshotValue = restored === null
      ? { selection: null, availability: "unselected" }
      : { selection: restored, availability: "unavailable", reason: "target has not been checked yet" };
    this.publishAvailableSelection();
  }

  async refresh(signal?: AbortSignal): Promise<PhuxTargetSnapshot> {
    signal?.throwIfAborted();
    try {
      const result = await this.inventory.agentList(signal === undefined ? {} : { signal });
      signal?.throwIfAborted();
      this.panesValue = result.agents;
      const selection = this.snapshotValue.selection;
      if (selection === null) {
        this.snapshotValue = { selection: null, availability: "unselected" };
      } else {
        const pane = result.agents.find((candidate) => candidate.terminal === selection.selector);
        if (pane === undefined) {
          this.snapshotValue = {
            selection,
            availability: "stale",
            reason: `pane ${selection.selector} is no longer present`,
          };
        } else if (pane.session !== selection.session || pane.window !== selection.window) {
          this.snapshotValue = {
            selection,
            availability: "stale",
            reason: `pane ${selection.selector} now belongs to ${pane.session}:${pane.window}; expected ${selection.session}:${selection.window}`,
          };
        } else {
          this.snapshotValue = { selection, availability: "available" };
        }
      }
    } catch (error) {
      signal?.throwIfAborted();
      this.panesValue = [];
      this.snapshotValue = {
        selection: this.snapshotValue.selection,
        availability: "unavailable",
        reason: error instanceof Error ? error.message : String(error),
      };
    }
    this.publishAvailableSelection();
    return this.snapshotValue;
  }

  select(pane: AgentPane): PhuxTargetSelection {
    return this.persist({
      version: PHUX_TARGET_VERSION,
      selector: pane.terminal,
      session: pane.session,
      window: pane.window,
      display: formatPaneDisplay(pane),
    });
  }

  bindAlias(name: string, selection: PhuxTargetSelection): void {
    requireTargetName(name);
    this.namedValue = {
      aliases: { ...this.namedValue.aliases, [name]: selection },
      groups: this.namedValue.groups,
    };
    this.persistNamed();
  }

  removeAlias(name: string): void {
    requireTargetName(name);
    const { [name]: _removed, ...aliases } = this.namedValue.aliases;
    this.namedValue = { aliases, groups: this.namedValue.groups };
    this.persistNamed();
  }

  bindGroup(name: string, selections: readonly PhuxTargetSelection[]): void {
    requireTargetName(name);
    if (selections.length === 0 || selections.length > 64) {
      throw new RangeError("a target group must contain 1 through 64 panes");
    }
    const unique = [...new Map(selections.map((selection) => [selection.selector, selection])).values()];
    this.namedValue = {
      aliases: this.namedValue.aliases,
      groups: { ...this.namedValue.groups, [name]: unique },
    };
    this.persistNamed();
  }

  removeGroup(name: string): void {
    requireTargetName(name);
    const { [name]: _removed, ...groups } = this.namedValue.groups;
    this.namedValue = { aliases: this.namedValue.aliases, groups };
    this.persistNamed();
  }

  selectionFor(selector: string): PhuxTargetSelection | null {
    const pane = this.panesValue.find((candidate) => candidate.terminal === selector);
    return pane === undefined ? null : selectionFromPane(pane);
  }

  resolveAlias(name: string): PhuxTargetSelection {
    requireTargetName(name);
    const selection = this.namedValue.aliases[name];
    if (selection === undefined) throw new Error(`Unknown phux target alias ${JSON.stringify(name)}.`);
    this.requireAvailable(selection, `alias:${name}`);
    return selection;
  }

  resolveGroup(name: string): readonly PhuxTargetSelection[] {
    requireTargetName(name);
    const selections = this.namedValue.groups[name];
    if (selections === undefined) throw new Error(`Unknown phux target group ${JSON.stringify(name)}.`);
    for (const selection of selections) this.requireAvailable(selection, `group:${name}`);
    return selections;
  }

  /** Select the documented seed pane created by `phux new --json`. */
  selectCreated(session: string, terminalId: number): PhuxTargetSelection {
    const selector = `@${String(terminalId)}`;
    return this.persist({
      version: PHUX_TARGET_VERSION,
      selector,
      session,
      window: "window-0",
      display: `${session}:window-0 ${selector}`,
    });
  }

  private requireAvailable(selection: PhuxTargetSelection, label: string): void {
    const pane = this.panesValue.find((candidate) => candidate.terminal === selection.selector);
    if (pane === undefined) throw new Error(`Named phux target ${label} is stale: pane ${selection.selector} is no longer present.`);
    if (pane.session !== selection.session || pane.window !== selection.window) {
      throw new Error(`Named phux target ${label} is stale: pane ${selection.selector} ownership changed.`);
    }
  }

  private persistNamed(): void {
    const data: PersistedNamedTargets = {
      version: PHUX_NAMED_TARGETS_VERSION,
      aliases: this.namedValue.aliases,
      groups: this.namedValue.groups,
    };
    this.persistence.appendEntry(PHUX_NAMED_TARGETS_ENTRY, data);
  }

  private persist(selection: PhuxTargetSelection): PhuxTargetSelection {
    this.persistence.appendEntry(PHUX_TARGET_ENTRY, selection);
    this.snapshotValue = { selection, availability: "available" };
    this.publishAvailableSelection();
    return selection;
  }

  private publishAvailableSelection(): void {
    const selection = this.snapshotValue.availability === "available"
      ? this.snapshotValue.selection
      : null;
    if (sameSelection(this.publishedSelection, selection)) return;
    this.publishedSelection = selection;
    for (const listener of this.listeners) listener(selection);
  }
}

function sameSelection(
  left: PhuxTargetSelection | null,
  right: PhuxTargetSelection | null,
): boolean {
  if (left === null || right === null) return left === right;
  return left.selector === right.selector && left.session === right.session && left.window === right.window;
}

function selectionFromPane(pane: AgentPane): PhuxTargetSelection {
  return {
    version: PHUX_TARGET_VERSION,
    selector: pane.terminal,
    session: pane.session,
    window: pane.window,
    display: formatPaneDisplay(pane),
  };
}

export function formatPaneDisplay(pane: AgentPane): string {
  const agent = pane.agent.label.trim();
  return `${pane.session}:${pane.window} ${pane.terminal}${agent.length === 0 ? "" : ` - ${agent}`}`;
}

export function formatTargetStatus(snapshot: PhuxTargetSnapshot): string {
  const selection = snapshot.selection;
  if (selection === null) {
    return snapshot.availability === "unavailable" ? "phux: unavailable" : "phux: no target";
  }
  const suffix = snapshot.availability === "available" ? "" : ` (${snapshot.availability})`;
  return `phux: ${selection.display}${suffix}`;
}

function requireTargetName(name: string): void {
  if (!PHUX_TARGET_NAME_PATTERN.test(name)) {
    throw new TypeError("target names must start with a letter and contain only letters, digits, _ or - (maximum 64 characters)");
  }
}

function parseNamedTargets(value: unknown): PhuxNamedTargetsSnapshot {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return { aliases: {}, groups: {} };
  const row = value as Record<string, unknown>;
  if (row.version !== PHUX_NAMED_TARGETS_VERSION || row.aliases === null || typeof row.aliases !== "object" || Array.isArray(row.aliases) || row.groups === null || typeof row.groups !== "object" || Array.isArray(row.groups)) {
    return { aliases: {}, groups: {} };
  }
  const aliases: Record<string, PhuxTargetSelection> = {};
  for (const [name, raw] of Object.entries(row.aliases as Record<string, unknown>)) {
    const parsed = parseSelection(raw);
    if (PHUX_TARGET_NAME_PATTERN.test(name) && parsed !== null) aliases[name] = parsed;
  }
  const groups: Record<string, readonly PhuxTargetSelection[]> = {};
  for (const [name, raw] of Object.entries(row.groups as Record<string, unknown>)) {
    if (!PHUX_TARGET_NAME_PATTERN.test(name) || !Array.isArray(raw) || raw.length === 0 || raw.length > 64) continue;
    const parsed = raw.map(parseSelection);
    if (parsed.every((selection) => selection !== null)) groups[name] = parsed as PhuxTargetSelection[];
  }
  return { aliases, groups };
}

function parseSelection(value: unknown): PhuxTargetSelection | null {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
  const row = value as Record<string, unknown>;
  if (row.version !== PHUX_TARGET_VERSION ||
      typeof row.selector !== "string" || row.selector.length === 0 ||
      typeof row.session !== "string" || row.session.length === 0 ||
      typeof row.window !== "string" || row.window.length === 0 ||
      typeof row.display !== "string" || row.display.length === 0) {
    return null;
  }
  return {
    version: PHUX_TARGET_VERSION,
    selector: row.selector,
    session: row.session,
    window: row.window,
    display: row.display,
  };
}
