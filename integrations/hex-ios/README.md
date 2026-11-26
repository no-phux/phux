# Talk to your phux chief from the iPhone Action Button

Press and hold the Action Button to start Hex recording. Speak, then press and
hold it again to stop and send. Hex shows its recording Live Activity and
transcribes on the phone. The Shortcut sends the text as one prompt to the
dedicated OpenCode pane in phux, then displays a delivery receipt.

The first press has no transcript and makes no network request. Use the
Action Button to finish a recording you want to send: stopping from Hex's
Live Activity only finishes the dictation in Hex. Find the chief's reply in
the `voice-chief` session. **Queued** means phux acknowledged the pane input.

## Set up the Mac

Use an installed phux that supports [acknowledged agent
prompts](../../docs/agents.md#driving-an-agent-in-a-pane), OpenCode with its
phux integration, Python 3, `jq`, a Mac and iPhone on the same tailnet,
and the [Hex iPhone app](https://apps.apple.com/us/app/hex/id6836837368).
Set up Hex's microphone and speech model on the iPhone first.

On the phux host, create a dedicated pane that runs OpenCode directly. Put
your orchestration instructions in `workspace/AGENTS.md`.

```sh
state="$HOME/.local/share/phux/hex-ios"
umask 077
mkdir -p "$state/workspace"
pane=$(phux new --json -s voice-chief -c "$state/workspace" -- "$(command -v opencode)" |
  jq -er '"@" + (.terminal_id | tostring)')
pane_key=$(phux ls --json | jq -er --arg pane "$pane" '.resources[] | select(.id == $pane) | .key')
(set -C; python3 -c 'import secrets; print(secrets.token_urlsafe(32))' > "$state/token")
```

Run the bridge from the host account that owns the local phux server. For a
launch agent, use absolute paths and pass that server's socket explicitly.

```sh
python3 integrations/hex-ios/serve.py \
  --phux "$(command -v phux)" --pane-key "$pane_key" \
  --token-file "$state/token"
```

In another shell, expose just this loopback listener through Tailscale Serve:

```sh
tailscale serve --bg --set-path /hex-chief http://127.0.0.1:18468
```

Keep it tailnet-only. The bridge takes a bearer token, never accepts a target
from the phone, and does not log transcripts. It checks the pane's stable key
and the declared OpenCode kind. If you create a different pane for the chief,
update the bridge's `--pane-key`. If phux reports the chief working or blocked,
the bridge refuses the prompt. If delivery is unknown, check the chief before
repeating it.

## Add the Shortcut

Create **Phux Chief** in Shortcuts with the following actions:

1. **Dictate with Hex**. Use this action's result as the transcript.
2. **If** the transcript **has any value**:
   - **Get Contents of URL**: `https://YOUR-MAC.YOUR-TAILNET.ts.net/hex-chief/`,
     method **POST**. Add the `Authorization` header with value `Bearer ` followed
     by your private token. Set **Request Body** to **JSON** and add the text field
     `text`, with the **Dictate with Hex** result as its value.
   - **Show Notification**: title **Phux Chief**, body **Contents of URL**.

[`phux-chief.cherri`](./phux-chief.cherri) is the same Shortcut in source
form. To generate an importable file, use
[Cherri with App Intent declarations](https://github.com/electrikmilk/cherri/tree/98d5191ae121a6bcf663ed0e55495f4ee4c7a6e2).
Fill in the URL and token in a private copy, then on the Mac:

```sh
cherri phux-chief.cherri --skip-sign
plutil -replace WFWorkflowActions.0.WFWorkflowActionParameters.AppIntentDescriptor.TeamIdentifier \
  -string 5NZ4Q7NXJ4 'Phux Chief_unsigned.shortcut'
shortcuts sign --mode people-who-know-me --input 'Phux Chief_unsigned.shortcut' \
  --output 'Phux Chief.shortcut'
```

Transfer the signed file to your iPhone and open it in Shortcuts. It contains
the token; keep it within your devices.

On iPhone, select **Settings → Action Button → Shortcut → Phux Chief**.
Allow Shortcuts to connect to the tailnet host when iOS asks. If prompted
to unlock, unlock and run the Shortcut from the button again.

## Check the bridge

```sh
python3 -B -m unittest discover -s integrations/hex-ios -v
```
