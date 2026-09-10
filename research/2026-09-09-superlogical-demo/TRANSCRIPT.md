---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-09-09
---

# Superlogical remote-host demo transcript

**TL;DR.** Timestamped, machine-generated English narration for the complete
6:16 demo. Product and technical spellings are normalized; raw recognition is
preserved separately. This is a transcript, not a verified verbatim quotation.
Segment boundaries are automatic and can fall mid-sentence.

## Provenance

Generated locally with mlx-whisper and whisper-large-v3-turbo from the supplied
MP4 audio. No subtitle stream is present. The direct media URL provides no
caption discovery endpoint; no author-supplied captions were recovered.
See [source metadata](source.json), [raw recognition](transcript-raw.json),
[WebVTT captions](transcript.vtt), and [the reference viewer](index.html).

Normalized terms: SuperLogical → Superlogical; tail net → tailnet;
tail scale → Tailscale; MOSH → Mosh; Who Am I → whoami;
GoToDirectory → Go to Directory. Other awkward recognition, such as
“I dropped this up” near 03:13, is retained rather than silently guessed.
No separate human listening pass was performed. Identity names and exact
quotations should be checked against the audio before publication.

## Narration

**00:00.800–00:04.720** All right, here's a short demo showing Superlogical working with remote hosts.

**00:05.240–00:08.480** This is going to be a short demo, not going to dive into details of everything,

**00:08.640–00:11.800** not going to explain how everything works, but I hope you enjoy it.

**00:11.820–00:14.680** I hope you get an idea about how this stuff is going to look.

**00:15.160–00:19.420** So first, when I start up, I'm just in my local application, local session.

**00:19.900–00:23.040** Of course, even though it's local, it's persistent, so I could type,

**00:23.320–00:25.260** and I could reopen the application, and that's saved.

**00:26.020–00:27.580** But I could also connect to remote hosts.

**00:27.580–00:30.660** We don't just want persistent sessions locally, we want them remotely.

**00:30.980–00:35.320** So let me open the command palette, add a remote host, and I have some infrastructure here.

**00:35.800–00:40.580** This is actually a VPS that's running remotely that's connected to my tailnet,

**00:40.820–00:43.400** so I could connect to that, and we're in.

**00:44.400–00:46.320** And, you know, it looks like an SSH session.

**00:46.940–00:51.320** But I could open a split, and I have two now, and this is over the same connection.

**00:51.860–00:55.200** And I also want to highlight that this is a real login.

**00:55.200–00:58.760** So if I list my logins, this is a real thing.

**00:59.500–01:00.440** Rex is the multiplexer name.

**01:00.720–01:03.800** So this isn't just a terminal opening via our server.

**01:04.100–01:07.960** Our server does a full SSH-style system login.

**01:08.300–01:15.340** So that shows up who works, all of your login shell stuff is loaded,

**01:15.540–01:22.200** all of your per-user limits are honored and respected on your Linux machine, etc., etc., etc.

**01:22.200–01:25.880** This is a full login, and actually Rex is a full SSH replacement,

**01:26.000–01:28.160** but I'm not going to talk too much about that here.

**01:28.940–01:31.160** I actually don't have SSH on this machine at all.

**01:31.540–01:32.520** This is how I get in.

**01:32.700–01:34.800** But, yeah, so let's create a split.

**01:35.060–01:35.600** Let's start there.

**01:36.020–01:40.420** Next thing, if you look up here, the machine showed up in our session list,

**01:40.660–01:43.680** and we got the new session, and every new session gets a unique name.

**01:43.740–01:44.600** This is Drifting Cedar.

**01:45.140–01:46.300** We could just rename it.

**01:46.400–01:48.840** So I could rename this session, and then say it's the demo session.

**01:49.360–01:50.280** And there you go, it's the demo.

**01:50.960–01:52.700** Let's go hop back to our Mac.

**01:52.960–01:55.780** So we're back on the Mac, and I want to actually go to the CLI.

**01:55.880–01:58.500** So every terminal session gets this injected.

**01:58.660–02:00.700** So on my local machine, I have it.

**02:00.760–02:03.580** If I go back to this one, it's also installed here.

**02:05.000–02:06.500** But let's go back up here.

**02:06.900–02:10.260** And the first thing I want to do is actually not this.

**02:10.260–02:12.640** I want to do what is it called, like whoami?

**02:13.280–02:15.180** The whoami tells you who you're logged in as.

**02:15.180–02:21.860** So I'm connecting, I'm on my Mac, connecting to Hill, and it's telling me who I am and how I got there.

**02:22.040–02:24.840** So I connected as MitchellH at GitHub.

**02:25.180–02:26.560** That's my Tailscale identity.

**02:26.560–02:28.780** Since I connected over Tailscale, that's how I got my identity.

**02:29.220–02:34.880** Over Tailscale via a couple hops from this machine, and I'm acting as MitchellH.

**02:35.220–02:40.300** I did hint earlier that this is a full SSH replacement, so I could also log in as root.

**02:40.900–02:41.420** And that's root.

**02:41.420–02:49.260** Like I said, I'm not going to get into the full details of the SSH replacement thing, but there is a mapping on the server side of who can act as who and how they authenticate.

**02:49.480–02:52.320** And we respect preexisting SSH keys and everything.

**02:52.440–02:54.020** So this is all just working.

**02:54.920–02:56.440** So I could log in as both.

**02:57.040–02:59.700** But we could do some other fun stuff with the CLI that I want to show you.

**02:59.840–03:03.020** So if we look, we have the demo session, right?

**03:03.020–03:08.400** Okay, but here on my Mac, I could create a new session, and Crisp Sierra has showed up.

**03:09.060–03:11.440** If I go there, you could see that it's empty.

**03:11.520–03:11.840** It's new.

**03:11.980–03:12.780** I could open a tab.

**03:13.480–03:18.040** But it's clearly different from the demo where, you know, I dropped this up.

**03:18.100–03:20.840** Let's just put some things in here so we could see, and let's go root.

**03:22.260–03:27.160** The other thing I want you to notice is as I'm typing, it's hard to tell, of course, over video, but as I'm typing, this is really responsive.

**03:27.160–03:34.840** This is a remote server that's actually across the country for me, and it feels local.

**03:35.640–03:43.780** We bring in a lot of the architectural similarities as Mosh into this, and so it's going to continue to get better.

**03:43.860–03:47.220** We don't do everything Mosh does, but I get asked a lot if we do some stuff, and we do.

**03:47.600–03:55.040** So I'm not ready to go into details exactly of the full protocol and transport, but it's super responsive.

**03:55.040–04:05.260** The other thing I want to notice is if I quit, quit my local app, it'll be a little slower to load this time, but we reconnect, and we go back into the last session we were in.

**04:05.440–04:07.540** So I immediately went in, and we're still here.

**04:08.120–04:10.120** And, of course, all our sessions are still here.

**04:12.000–04:21.020** Next, if I grab this ID here, I could actually go ahead and do a session kill, and I want you to notice how fast this is.

**04:21.080–04:22.000** So we have this here.

**04:22.320–04:23.380** I'm talking to this server.

**04:23.380–04:26.520** If I hit go and reopen, it's gone already.

**04:26.960–04:28.600** All this stuff is super fast.

**04:28.700–04:30.640** It propagates to all the clients really quickly.

**04:31.580–04:37.340** And, again, this is all about bringing down the barrier so that everything feels local.

**04:37.880–04:42.780** The barrier between local and remote has to disappear.

**04:43.000–04:47.820** It all has to feel like it's right at your fingertips, and that's built in all the way through.

**04:49.100–04:51.340** Okay, I want to show one other kind of fun feature.

**04:51.340–04:56.680** So if I go over here, we added a feature where if you do Command-Shift-G, we could jump.

**04:56.860–04:58.100** It's a Go to Directory feature.

**04:58.100–05:03.560** So we could go ahead and jump, let's say, to the macOS folder, and it opens a new terminal in that folder.

**05:03.700–05:04.680** So you could see that we're here.

**05:05.660–05:06.740** Okay, makes sense, right?

**05:07.180–05:13.700** Well, the cool part is if we go ahead and go over here, I'm now on my remote machine in the root directory, and I do that again.

**05:14.060–05:14.780** It works.

**05:15.320–05:18.000** Go to Directory works over remote connections as well.

**05:18.000–05:19.000** So I could go to proc.

**05:19.080–05:20.400** It lists the things in proc.

**05:20.620–05:21.580** I could go to home.

**05:21.680–05:22.620** We could see some users.

**05:22.780–05:25.580** I could go back to Mitchell H, hit enter, and there we go.

**05:25.980–05:26.660** We're in there.

**05:27.660–05:32.920** Again, this is all about dropping the boundary of local versus remote.

**05:33.060–05:34.920** Everything that works local should work remote.

**05:35.020–05:36.520** Everything that works remote should work local.

**05:36.520–05:42.400** And that permeates through everything in our design here and functionality.

**05:43.880–05:48.380** The way that Go to Directory works over remote connections is really cool.

**05:48.980–05:54.280** We're going to have to wait for future details of how our architecture works to enable things like this.

**05:54.680–05:58.380** And we'll continue to show more functionality as we get closer.

**05:58.600–06:01.960** But that's a demo of remote hosts.

**06:01.960–06:07.040** Some hints towards some really cool stuff like Rex being an SSH replacement.

**06:07.740–06:12.000** Some sneaky little protocol things in there for both performance and functionality.

**06:12.820–06:13.660** There's a lot here.

**06:13.820–06:16.140** So hope you liked that demo and talk to you soon.
