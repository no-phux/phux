import { useEffect, useState } from "react";
import PhuxTerminal from "./PhuxTerminal";
import type { SessionPair } from "./terminal/core";
import "./PhonePair.css";

/**
 * The phone half of the pairing link: read the single-use code out of the URL
 * fragment (it never reached a server, and never reaches one now), then attach
 * this screen to the session that minted it.
 *
 * The code is spent by the attachment itself, so there is no "connect" button to
 * press and nothing to install: open the link and the shell is there.
 */
export default function PhonePair({ wsUrl = "" }: { wsUrl?: string }) {
  const [pair, setPair] = useState<SessionPair | null>(null);
  const [reading, setReading] = useState(true);

  useEffect(() => {
    const code = new URLSearchParams(window.location.hash.slice(1)).get("code")
      ?? window.location.hash.slice(1);
    // Drop it from the visible URL: the code is single-use, and its only
    // lifetime should be this attachment.
    history.replaceState({}, "", window.location.pathname);
    if (code) {
      const expiresAt = Date.now() + 5 * 60_000;
      setPair({ code, expiresAt });
    }
    setReading(false);
  }, []);

  if (reading) {
    return <p className="phone-note" role="status">Reading your pairing link…</p>;
  }
  if (!pair) {
    return (
      <p className="phone-note" role="status">
        This link is missing its pairing code. Open the demo on phux.sh and scan the code there,
        or paste the link as printed.
      </p>
    );
  }

  // A build with no hosted-shell endpoint has nothing to attach to. Say so
  // rather than failing to parse an absent URL.
  let endpoint: string;
  try {
    const url = new URL(wsUrl);
    url.searchParams.set("attach", pair.code);
    endpoint = url.toString();
  } catch {
    return (
      <p className="phone-note" role="status">
        Hosted shells are not configured on this site, so this pairing link has
        nothing to attach to.
      </p>
    );
  }

  return (
    <PhuxTerminal
      wsUrl={endpoint}
      mode="demo"
      cols={46}
      rows={18}
      autoStart
      focusOnStart
      headerActions={<span className="phone-title">Same shell as your screen</span>}
    />
  );
}
