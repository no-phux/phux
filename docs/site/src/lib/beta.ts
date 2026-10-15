// TestFlight access requests for phux-mobile, shared by the landing and /more.
//
// POST /api/beta reaches host/analytics.ts -> host/testflight.ts, which adds
// the address to the App Store Connect external group (Apple then emails the
// invite) and still records the request through the member pipeline when the
// Worker holds no App Store Connect secrets. `website` is the honeypot the
// Worker drops before touching TestFlight.

type BetaResponse = {
  ok?: boolean;
  status?: string;
  error?: string;
};

/**
 * Wire one TestFlight request form. Safe to call with a missing form (the
 * landing and /more each render exactly one), and never throws: a network or
 * parse failure surfaces in the note instead.
 */
export function wireBetaRequest(
  form: HTMLFormElement | null,
  note: HTMLElement | null,
): void {
  if (!form) return;
  const email = form.elements.namedItem("email") as HTMLInputElement | null;
  const trap = form.elements.namedItem("website") as HTMLInputElement | null;
  const button = form.querySelector<HTMLButtonElement>("button[type=submit]");

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    void submit(form, email, trap, button, note);
  });
}

async function submit(
  form: HTMLFormElement,
  email: HTMLInputElement | null,
  trap: HTMLInputElement | null,
  button: HTMLButtonElement | null,
  note: HTMLElement | null,
): Promise<void> {
  const value = email?.value.trim() ?? "";
  if (button) button.disabled = true;
  try {
    const response = await fetch("/api/beta", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ email: value, website: trap?.value ?? "" }),
    });
    const result = (await response.json()) as BetaResponse;
    if (!result.ok) {
      if (note) {
        note.textContent =
          "Could not request access: " + (result.error ?? "try again.");
      }
      return;
    }
    if (note) {
      note.textContent =
        result.status === "invited"
          ? "Invite sent. Check " + value + " for an email from TestFlight."
          : "Request received. The TestFlight invite will go to " + value + ".";
    }
    form.reset();
  } catch {
    if (note) note.textContent = "Could not connect. Try again.";
  } finally {
    if (button) button.disabled = false;
  }
}
