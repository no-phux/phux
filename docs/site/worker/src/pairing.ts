// Pairing admission: whether one attach code may still buy a second screen.
//
// A session mints exactly one code, hands it to the view that opened the session
// (and nobody else), and spends it on first use. These are the rules the session
// object applies at the moment of attachment, kept pure so they can be tested
// without a live object.

/** How long a pairing code stays spendable, and never past the session itself. */
export const PAIR_TTL_MS = 5 * 60_000;

export interface PairRecord {
  code: string;
  expiresAt: number;
  /** Set when a second screen spent the code; a code is spendable once. */
  claimedAt?: number;
}

/**
 * Why this pairing code cannot be spent, or null when it can. The caller has
 * already proved the code is one this session minted, so these reasons describe
 * state, not identity.
 */
export function pairRejection(
  pair: PairRecord | undefined,
  presented: string,
  now: number,
): string | null {
  if (!pair || pair.claimedAt !== undefined) {
    return "this pairing link was already used";
  }
  if (pair.expiresAt <= now) return "this pairing link has expired";
  if (pair.code !== presented) return "this pairing link is not valid";
  return null;
}
