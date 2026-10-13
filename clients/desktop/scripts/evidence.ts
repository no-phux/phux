/**
 * Smoke evidence must come from a presented frame. A harness-written ok flag
 * is not success, and a layout that was not presented is not a capture.
 */
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

const PNG = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

export interface PlacementEvidence {
  id: string;
  terminalId: string;
}

/**
 * `confirmed`: the display reported a nonzero presented time for the exact
 * drawable. `submitted`: that drawable reached a visible window, but the
 * display could not confirm it (a hosted CI virtual display never does).
 */
export type Presentation = "confirmed" | "submitted";

export interface LayoutEvidence {
  session: string;
  presentation: Presentation;
  generation: number;
  placements: PlacementEvidence[];
}

export interface InteractionEvidence {
  kind: "present" | "key" | "click" | "scroll";
  presentedGeneration: number;
  visible: true;
  clipped: false;
  bounds: { width: number; height: number };
}

function record(value: unknown): Record<string, unknown> | undefined {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return undefined;
  return Object.fromEntries(Object.entries(value));
}

function readJson(path: string): unknown {
  return JSON.parse(readFileSync(path, "utf8"));
}

/**
 * `allowSubmitted` accepts a drawable the display could not confirm. Pass it
 * only where no real display exists; everywhere else confirmation is the bar.
 */
export function assertPresentedEvidence(
  dir: string,
  session: string,
  allowSubmitted = false,
): LayoutEvidence {
  const image = join(dir, "presented.png");
  if (!existsSync(image)) throw new Error(`${dir}: presented.png is missing`);
  const bytes = readFileSync(image);
  if (bytes.length <= 24 || !bytes.subarray(0, 8).equals(PNG)) {
    throw new Error(`${dir}: presented.png is not a PNG capture`);
  }
  const layout = parseLayout(readJson(join(dir, "layout.json")), session, allowSubmitted);
  const interaction = parseInteraction(readJson(join(dir, "interaction.json")));
  if (interaction.presentedGeneration !== layout.generation) {
    throw new Error(`${dir}: interaction generation does not match the presented layout`);
  }
  return layout;
}

function presentation(layout: Record<string, unknown>, allowSubmitted: boolean): Presentation {
  if (layout.presented === true) return "confirmed";
  if (allowSubmitted && layout.submitted === true) return "submitted";
  throw new Error("layout was not presented");
}

function parseLayout(value: unknown, session: string, allowSubmitted: boolean): LayoutEvidence {
  const layout = record(value);
  if (!layout) throw new Error("layout was not presented");
  const shown = presentation(layout, allowSubmitted);
  if (layout.session !== session) throw new Error(`layout session is not ${session}`);
  if (typeof layout.generation !== "number" || layout.generation <= 0) {
    throw new Error("layout generation must be a presented frame");
  }
  if (!Array.isArray(layout.placements) || layout.placements.length === 0) {
    throw new Error("layout has no placements");
  }
  const placements = layout.placements.map(parsePlacement);
  return { session, presentation: shown, generation: layout.generation, placements };
}

function parsePlacement(value: unknown): PlacementEvidence {
  const placement = record(value);
  if (!placement || typeof placement.id !== "string" || placement.id === "") {
    throw new Error("placement is missing an id");
  }
  if (typeof placement.terminalId !== "string" || placement.terminalId === "") {
    throw new Error("placement is missing a terminal");
  }
  return { id: placement.id, terminalId: placement.terminalId };
}

function parseInteraction(value: unknown): InteractionEvidence {
  const interaction = record(value);
  if (!interaction) throw new Error("interaction evidence is missing");
  if (
    interaction.kind !== "present" &&
    interaction.kind !== "key" &&
    interaction.kind !== "click" &&
    interaction.kind !== "scroll"
  ) {
    throw new Error("interaction kind is not a runtime event");
  }
  if (typeof interaction.presentedGeneration !== "number" || interaction.presentedGeneration <= 0) {
    throw new Error("interaction has no presented generation");
  }
  if (interaction.visible !== true || interaction.clipped !== false) {
    throw new Error("interaction was not an unclipped visible presentation");
  }
  const bounds = record(interaction.bounds);
  if (
    !bounds ||
    typeof bounds.width !== "number" ||
    typeof bounds.height !== "number" ||
    bounds.width <= 0 ||
    bounds.height <= 0
  ) {
    throw new Error("interaction bounds are empty");
  }
  return {
    kind: interaction.kind,
    presentedGeneration: interaction.presentedGeneration,
    visible: true,
    clipped: false,
    bounds: { width: bounds.width, height: bounds.height },
  };
}
