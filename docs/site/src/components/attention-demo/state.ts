export type AttentionDemoPhase = "working" | "blocked" | "resolved";

export const ATTENTION_DEMO_SEQUENCE: ReadonlyArray<{
  phase: AttentionDemoPhase;
  durationMs: number;
}> = [
  { phase: "working", durationMs: 3_500 },
  { phase: "blocked", durationMs: 7_000 },
  { phase: "resolved", durationMs: 3_000 },
];

export function nextAttentionDemoPhase(phase: AttentionDemoPhase): AttentionDemoPhase {
  const index = ATTENTION_DEMO_SEQUENCE.findIndex((step) => step.phase === phase);
  return ATTENTION_DEMO_SEQUENCE[(index + 1) % ATTENTION_DEMO_SEQUENCE.length].phase;
}

export function attentionDemoDuration(phase: AttentionDemoPhase): number {
  return ATTENTION_DEMO_SEQUENCE.find((step) => step.phase === phase)!.durationMs;
}
