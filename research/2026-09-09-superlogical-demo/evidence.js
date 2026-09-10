// Generated from frames.json and terminology-normalized transcript-raw.json.
window.evidence = {
  "frames": [
    {
      "id": "S01",
      "time": 16,
      "title": "Local session baseline",
      "note": "Session selector, tab strip, shell pane and pane toolbar."
    },
    {
      "id": "S02",
      "time": 21,
      "title": "Local input before quitting",
      "note": "Typed input provides the persistence marker."
    },
    {
      "id": "S03",
      "time": 25,
      "title": "Local input restored",
      "note": "Input remains after application close/reopen sequence."
    },
    {
      "id": "S04",
      "time": 32,
      "title": "Filtered command palette",
      "note": "Add Remote Host selected; pane, density and session actions also visible."
    },
    {
      "id": "S05",
      "time": 38,
      "title": "Add remote host dialog",
      "note": "Hostname entry and connection action; populated with a tailnet hostname."
    },
    {
      "id": "S06",
      "time": 44,
      "title": "Connected remote shell",
      "note": "Remote session opens in the same window structure as local."
    },
    {
      "id": "S07",
      "time": 50,
      "title": "Remote split panes",
      "note": "Two side-by-side shells; narration says one connection."
    },
    {
      "id": "S08",
      "time": 59,
      "title": "System login evidence",
      "note": "Login command output includes Rex; full login semantics are narrated."
    },
    {
      "id": "S09",
      "time": 99,
      "title": "Host-grouped session list",
      "note": "Remote machine appears as a group with an automatically named session."
    },
    {
      "id": "S10",
      "time": 106,
      "title": "Command palette session actions",
      "note": "Rename Session and Rename Tab appear in the global command palette."
    },
    {
      "id": "S11",
      "time": 107.75,
      "title": "Rename session dialog",
      "note": "Session name edit and confirmation row in a compact palette-style popover."
    },
    {
      "id": "S12",
      "time": 110,
      "title": "Renamed Demo session",
      "note": "Renamed session appears in the active window header."
    },
    {
      "id": "S13",
      "time": 120,
      "title": "Local Rex CLI help",
      "note": "Rex command-line interface available inside the local terminal."
    },
    {
      "id": "S14",
      "time": 122.5,
      "title": "Remote Rex CLI help",
      "note": "CLI is also available inside the remote terminal."
    },
    {
      "id": "S15",
      "time": 143,
      "title": "Remote identity from local CLI",
      "note": "whoami output describes authenticated identity, route and effective user."
    },
    {
      "id": "S16",
      "time": 162,
      "title": "Alternate effective user",
      "note": "Second whoami invocation acts as root; narration describes server policy mapping."
    },
    {
      "id": "S17",
      "time": 188,
      "title": "CLI-created session appears",
      "note": "New session identifier in terminal; new named session in selector."
    },
    {
      "id": "S18",
      "time": 191,
      "title": "Empty remote session",
      "note": "Empty session message with New Tab action."
    },
    {
      "id": "S19",
      "time": 193,
      "title": "First tab in new session",
      "note": "Empty session becomes a shell after opening a tab."
    },
    {
      "id": "S20",
      "time": 202,
      "title": "Distinct remote pane state",
      "note": "Demo split keeps command output on left and different working directory on right."
    },
    {
      "id": "S21",
      "time": 238.5,
      "title": "Application quit",
      "note": "Desktop visible; this is a transition frame, not product chrome."
    },
    {
      "id": "S22",
      "time": 244,
      "title": "Remote state restored on relaunch",
      "note": "Returns to Demo with split layout and previous shell output."
    },
    {
      "id": "S23",
      "time": 260,
      "title": "Remote session kill command",
      "note": "Local CLI targets remote session by identifier."
    },
    {
      "id": "S24",
      "time": 266,
      "title": "Session removed from selector",
      "note": "Previously created session absent after kill; Demo remains."
    },
    {
      "id": "S25",
      "time": 295.5,
      "title": "Local Go to Directory picker",
      "note": "Path field, Go to row, directory entries and highlighted selection."
    },
    {
      "id": "S26",
      "time": 301,
      "title": "Local path drill-down",
      "note": "Directory selection is narrowed toward macOS source folder."
    },
    {
      "id": "S27",
      "time": 304,
      "title": "New local terminal at destination",
      "note": "A new tab opens with the chosen directory as working directory."
    },
    {
      "id": "S28",
      "time": 314.5,
      "title": "Remote Go to Directory picker",
      "note": "Same picker structure on remote filesystem rooted at slash."
    },
    {
      "id": "S29",
      "time": 319.5,
      "title": "Remote proc listing",
      "note": "Directory picker lists entries beneath remote /proc."
    },
    {
      "id": "S30",
      "time": 322,
      "title": "Remote home listing",
      "note": "Remote /home contains user directories."
    },
    {
      "id": "S31",
      "time": 326.5,
      "title": "Remote destination opened",
      "note": "Selected remote home directory opens as terminal context."
    },
    {
      "id": "S32",
      "time": 355,
      "title": "Rapid return to remote session",
      "note": "Session switching restores remote split view; local and remote share navigation."
    }
  ],
  "segments": [
    {
      "start": 0.8,
      "end": 4.72,
      "text": "All right, here's a short demo showing Superlogical working with remote hosts."
    },
    {
      "start": 5.24,
      "end": 8.48,
      "text": "This is going to be a short demo, not going to dive into details of everything,"
    },
    {
      "start": 8.64,
      "end": 11.8,
      "text": "not going to explain how everything works, but I hope you enjoy it."
    },
    {
      "start": 11.82,
      "end": 14.68,
      "text": "I hope you get an idea about how this stuff is going to look."
    },
    {
      "start": 15.16,
      "end": 19.42,
      "text": "So first, when I start up, I'm just in my local application, local session."
    },
    {
      "start": 19.900000000000002,
      "end": 23.04,
      "text": "Of course, even though it's local, it's persistent, so I could type,"
    },
    {
      "start": 23.32,
      "end": 25.26,
      "text": "and I could reopen the application, and that's saved."
    },
    {
      "start": 26.02,
      "end": 27.580000000000002,
      "text": "But I could also connect to remote hosts."
    },
    {
      "start": 27.58,
      "end": 30.659999999999997,
      "text": "We don't just want persistent sessions locally, we want them remotely."
    },
    {
      "start": 30.979999999999997,
      "end": 35.32,
      "text": "So let me open the command palette, add a remote host, and I have some infrastructure here."
    },
    {
      "start": 35.8,
      "end": 40.58,
      "text": "This is actually a VPS that's running remotely that's connected to my tailnet,"
    },
    {
      "start": 40.82,
      "end": 43.4,
      "text": "so I could connect to that, and we're in."
    },
    {
      "start": 44.4,
      "end": 46.32,
      "text": "And, you know, it looks like an SSH session."
    },
    {
      "start": 46.94,
      "end": 51.32,
      "text": "But I could open a split, and I have two now, and this is over the same connection."
    },
    {
      "start": 51.86,
      "end": 55.2,
      "text": "And I also want to highlight that this is a real login."
    },
    {
      "start": 55.2,
      "end": 58.760000000000005,
      "text": "So if I list my logins, this is a real thing."
    },
    {
      "start": 59.5,
      "end": 60.440000000000005,
      "text": "Rex is the multiplexer name."
    },
    {
      "start": 60.720000000000006,
      "end": 63.800000000000004,
      "text": "So this isn't just a terminal opening via our server."
    },
    {
      "start": 64.10000000000001,
      "end": 67.96000000000001,
      "text": "Our server does a full SSH-style system login."
    },
    {
      "start": 68.3,
      "end": 75.34,
      "text": "So that shows up who works, all of your login shell stuff is loaded,"
    },
    {
      "start": 75.54,
      "end": 82.2,
      "text": "all of your per-user limits are honored and respected on your Linux machine, etc., etc., etc."
    },
    {
      "start": 82.2,
      "end": 85.88000000000001,
      "text": "This is a full login, and actually Rex is a full SSH replacement,"
    },
    {
      "start": 86.0,
      "end": 88.16,
      "text": "but I'm not going to talk too much about that here."
    },
    {
      "start": 88.94,
      "end": 91.16,
      "text": "I actually don't have SSH on this machine at all."
    },
    {
      "start": 91.54,
      "end": 92.52000000000001,
      "text": "This is how I get in."
    },
    {
      "start": 92.7,
      "end": 94.8,
      "text": "But, yeah, so let's create a split."
    },
    {
      "start": 95.06,
      "end": 95.60000000000001,
      "text": "Let's start there."
    },
    {
      "start": 96.02000000000001,
      "end": 100.42,
      "text": "Next thing, if you look up here, the machine showed up in our session list,"
    },
    {
      "start": 100.66,
      "end": 103.68,
      "text": "and we got the new session, and every new session gets a unique name."
    },
    {
      "start": 103.74000000000001,
      "end": 104.60000000000001,
      "text": "This is Drifting Cedar."
    },
    {
      "start": 105.14,
      "end": 106.30000000000001,
      "text": "We could just rename it."
    },
    {
      "start": 106.4,
      "end": 108.84,
      "text": "So I could rename this session, and then say it's the demo session."
    },
    {
      "start": 109.36,
      "end": 110.28,
      "text": "And there you go, it's the demo."
    },
    {
      "start": 110.96000000000001,
      "end": 112.7,
      "text": "Let's go hop back to our Mac."
    },
    {
      "start": 112.96000000000001,
      "end": 115.78,
      "text": "So we're back on the Mac, and I want to actually go to the CLI."
    },
    {
      "start": 115.88000000000001,
      "end": 118.5,
      "text": "So every terminal session gets this injected."
    },
    {
      "start": 118.66,
      "end": 120.7,
      "text": "So on my local machine, I have it."
    },
    {
      "start": 120.76,
      "end": 123.58,
      "text": "If I go back to this one, it's also installed here."
    },
    {
      "start": 125.0,
      "end": 126.5,
      "text": "But let's go back up here."
    },
    {
      "start": 126.9,
      "end": 130.26,
      "text": "And the first thing I want to do is actually not this."
    },
    {
      "start": 130.26,
      "end": 132.64000000000001,
      "text": "I want to do what is it called, like whoami?"
    },
    {
      "start": 133.28,
      "end": 135.18,
      "text": "The whoami tells you who you're logged in as."
    },
    {
      "start": 135.18,
      "end": 141.86,
      "text": "So I'm connecting, I'm on my Mac, connecting to Hill, and it's telling me who I am and how I got there."
    },
    {
      "start": 142.04000000000002,
      "end": 144.84,
      "text": "So I connected as MitchellH at GitHub."
    },
    {
      "start": 145.18,
      "end": 146.56,
      "text": "That's my Tailscale identity."
    },
    {
      "start": 146.56,
      "end": 148.78,
      "text": "Since I connected over Tailscale, that's how I got my identity."
    },
    {
      "start": 149.22,
      "end": 154.88,
      "text": "Over Tailscale via a couple hops from this machine, and I'm acting as MitchellH."
    },
    {
      "start": 155.22,
      "end": 160.3,
      "text": "I did hint earlier that this is a full SSH replacement, so I could also log in as root."
    },
    {
      "start": 160.9,
      "end": 161.42000000000002,
      "text": "And that's root."
    },
    {
      "start": 161.42,
      "end": 169.26,
      "text": "Like I said, I'm not going to get into the full details of the SSH replacement thing, but there is a mapping on the server side of who can act as who and how they authenticate."
    },
    {
      "start": 169.48,
      "end": 172.32,
      "text": "And we respect preexisting SSH keys and everything."
    },
    {
      "start": 172.44,
      "end": 174.01999999999998,
      "text": "So this is all just working."
    },
    {
      "start": 174.92,
      "end": 176.44,
      "text": "So I could log in as both."
    },
    {
      "start": 177.04,
      "end": 179.7,
      "text": "But we could do some other fun stuff with the CLI that I want to show you."
    },
    {
      "start": 179.83999999999997,
      "end": 183.01999999999998,
      "text": "So if we look, we have the demo session, right?"
    },
    {
      "start": 183.02,
      "end": 188.4,
      "text": "Okay, but here on my Mac, I could create a new session, and Crisp Sierra has showed up."
    },
    {
      "start": 189.06,
      "end": 191.44,
      "text": "If I go there, you could see that it's empty."
    },
    {
      "start": 191.52,
      "end": 191.84,
      "text": "It's new."
    },
    {
      "start": 191.98000000000002,
      "end": 192.78,
      "text": "I could open a tab."
    },
    {
      "start": 193.48000000000002,
      "end": 198.04000000000002,
      "text": "But it's clearly different from the demo where, you know, I dropped this up."
    },
    {
      "start": 198.10000000000002,
      "end": 200.84,
      "text": "Let's just put some things in here so we could see, and let's go root."
    },
    {
      "start": 202.26000000000002,
      "end": 207.16000000000003,
      "text": "The other thing I want you to notice is as I'm typing, it's hard to tell, of course, over video, but as I'm typing, this is really responsive."
    },
    {
      "start": 207.16,
      "end": 214.84,
      "text": "This is a remote server that's actually across the country for me, and it feels local."
    },
    {
      "start": 215.64,
      "end": 223.78,
      "text": "We bring in a lot of the architectural similarities as Mosh into this, and so it's going to continue to get better."
    },
    {
      "start": 223.85999999999999,
      "end": 227.22,
      "text": "We don't do everything Mosh does, but I get asked a lot if we do some stuff, and we do."
    },
    {
      "start": 227.6,
      "end": 235.04,
      "text": "So I'm not ready to go into details exactly of the full protocol and transport, but it's super responsive."
    },
    {
      "start": 235.04,
      "end": 245.26,
      "text": "The other thing I want to notice is if I quit, quit my local app, it'll be a little slower to load this time, but we reconnect, and we go back into the last session we were in."
    },
    {
      "start": 245.44,
      "end": 247.54,
      "text": "So I immediately went in, and we're still here."
    },
    {
      "start": 248.12,
      "end": 250.12,
      "text": "And, of course, all our sessions are still here."
    },
    {
      "start": 252.0,
      "end": 261.02,
      "text": "Next, if I grab this ID here, I could actually go ahead and do a session kill, and I want you to notice how fast this is."
    },
    {
      "start": 261.08,
      "end": 262.0,
      "text": "So we have this here."
    },
    {
      "start": 262.32,
      "end": 263.38,
      "text": "I'm talking to this server."
    },
    {
      "start": 263.38,
      "end": 266.52,
      "text": "If I hit go and reopen, it's gone already."
    },
    {
      "start": 266.96,
      "end": 268.6,
      "text": "All this stuff is super fast."
    },
    {
      "start": 268.7,
      "end": 270.64,
      "text": "It propagates to all the clients really quickly."
    },
    {
      "start": 271.58,
      "end": 277.34,
      "text": "And, again, this is all about bringing down the barrier so that everything feels local."
    },
    {
      "start": 277.88,
      "end": 282.78,
      "text": "The barrier between local and remote has to disappear."
    },
    {
      "start": 283.0,
      "end": 287.82,
      "text": "It all has to feel like it's right at your fingertips, and that's built in all the way through."
    },
    {
      "start": 289.1,
      "end": 291.34,
      "text": "Okay, I want to show one other kind of fun feature."
    },
    {
      "start": 291.34,
      "end": 296.67999999999995,
      "text": "So if I go over here, we added a feature where if you do Command-Shift-G, we could jump."
    },
    {
      "start": 296.85999999999996,
      "end": 298.09999999999997,
      "text": "It's a Go to Directory feature."
    },
    {
      "start": 298.1,
      "end": 303.56,
      "text": "So we could go ahead and jump, let's say, to the macOS folder, and it opens a new terminal in that folder."
    },
    {
      "start": 303.70000000000005,
      "end": 304.68,
      "text": "So you could see that we're here."
    },
    {
      "start": 305.66,
      "end": 306.74,
      "text": "Okay, makes sense, right?"
    },
    {
      "start": 307.18,
      "end": 313.70000000000005,
      "text": "Well, the cool part is if we go ahead and go over here, I'm now on my remote machine in the root directory, and I do that again."
    },
    {
      "start": 314.06,
      "end": 314.78000000000003,
      "text": "It works."
    },
    {
      "start": 315.32000000000005,
      "end": 318.0,
      "text": "Go to Directory works over remote connections as well."
    },
    {
      "start": 318.0,
      "end": 319.0,
      "text": "So I could go to proc."
    },
    {
      "start": 319.08,
      "end": 320.4,
      "text": "It lists the things in proc."
    },
    {
      "start": 320.62,
      "end": 321.58,
      "text": "I could go to home."
    },
    {
      "start": 321.68,
      "end": 322.62,
      "text": "We could see some users."
    },
    {
      "start": 322.78,
      "end": 325.58,
      "text": "I could go back to Mitchell H, hit enter, and there we go."
    },
    {
      "start": 325.98,
      "end": 326.66,
      "text": "We're in there."
    },
    {
      "start": 327.66,
      "end": 332.92,
      "text": "Again, this is all about dropping the boundary of local versus remote."
    },
    {
      "start": 333.06,
      "end": 334.92,
      "text": "Everything that works local should work remote."
    },
    {
      "start": 335.02,
      "end": 336.52,
      "text": "Everything that works remote should work local."
    },
    {
      "start": 336.52,
      "end": 342.4,
      "text": "And that permeates through everything in our design here and functionality."
    },
    {
      "start": 343.88,
      "end": 348.38,
      "text": "The way that Go to Directory works over remote connections is really cool."
    },
    {
      "start": 348.97999999999996,
      "end": 354.28,
      "text": "We're going to have to wait for future details of how our architecture works to enable things like this."
    },
    {
      "start": 354.68,
      "end": 358.38,
      "text": "And we'll continue to show more functionality as we get closer."
    },
    {
      "start": 358.59999999999997,
      "end": 361.96,
      "text": "But that's a demo of remote hosts."
    },
    {
      "start": 361.96,
      "end": 367.03999999999996,
      "text": "Some hints towards some really cool stuff like Rex being an SSH replacement."
    },
    {
      "start": 367.73999999999995,
      "end": 372.0,
      "text": "Some sneaky little protocol things in there for both performance and functionality."
    },
    {
      "start": 372.82,
      "end": 373.65999999999997,
      "text": "There's a lot here."
    },
    {
      "start": 373.82,
      "end": 376.14,
      "text": "So hope you liked that demo and talk to you soon."
    }
  ]
};
