# phux vs Rex (Superlogical)

Rex is [Superlogical's](https://www.superlogical.com) terminal multiplexer. Their site says they are building "the multiplexer for all work," and that they will begin with a terminal multiplexer.

[phux and Rex](./compare.md#phux-and-rex) on the compare page is the short note. This page is the longer one. Rex facts were checked on 2026-11-02 against the Rex pages linked here. phux facts were checked the same day against this repository. Newer releases may differ.

## At a glance

| | Rex | phux |
| --- | --- | --- |
| What it is | A terminal multiplexer, and the start of "the multiplexer for all work." [Homepage](https://www.superlogical.com/). The 5 October 2026 announcement calls the macOS app "a drop-in replacement for whichever terminal you use today." [Public testing](https://www.superlogical.com/updates/public-testing-beginning) | A terminal multiplexer whose server you can drive from code. [README](../README.md) |
| License | Not stated in the public docs. On 2026-11-02 the [superlogical](https://github.com/superlogical) organization listed no public repository, so there was no `LICENSE` file to read. The 5 October announcement says OSS releases are still to come. | Apache-2.0. [LICENSE](../LICENSE) |
| Account | "Rex is and will always be free." No account, and you can self-host the servers. [Public testing](https://www.superlogical.com/updates/public-testing-beginning). The privacy page says Rex has no usage analytics, no crash reporting of its own, and no account or license checks. [Privacy](https://www.superlogical.com/rex/docs/reference/privacy) | No accounts and no hosted service. Each machine runs its own server. [Remote machines](./remote.md) |
| Install today | The macOS application and the Linux and FreeBSD servers are available to all testers. The macOS client includes a local server. Windows servers, and iOS, Linux, and Windows clients, exist at varying readiness and are not in that test. [Installation](https://www.superlogical.com/rex/docs/installation), [FreeBSD](https://www.superlogical.com/rex/docs/freebsd/installation) | macOS on Apple silicon, and Linux x86_64 or arm64 with glibc. On Windows the CLI and terminal UI run under WSL 2, and the native desktop uses that server. No Intel Mac build and no musl build. [Install](./install.md) |
| Config | Lua, in `init.lua`. A file is optional. [Configuration](https://www.superlogical.com/rex/docs/customize/config) | One TOML file: `~/.config/phux/config.toml`, or `$XDG_CONFIG_HOME/phux/config.toml`. A file is optional. [Configuration](./config.md) |
| From code | The `rex` CLI, `--json`, shell scripts, and Lua (`rex do`). [CLI](https://www.superlogical.com/rex/docs/automate/cli), [Shell scripting](https://www.superlogical.com/rex/docs/automate/shell-scripting) | Commands with JSON output, and an MCP server (`phux mcp`). There is no public SDK and no JSON socket. [Scripting](./scripting.md), [Build on phux](./building.md) |
| Names in a script | A session, window, block, or client has an id that never changes and is never reused. A label can change, and two things can share one. [CLI](https://www.superlogical.com/rex/docs/automate/cli) | A pane id (`@7`) names a pane that exists now. It can be reused after that pane is gone or the server restarts. A key lasts across restarts. [Names, ids, and keys](./concepts.md#names-ids-and-keys) |
| Another machine | `-S` / `--server` selects a host by label or address. A Linux or FreeBSD server listens on a Unix socket until you add a listener. `rex server start --tailscale` joins a tailnet with Tailscale built into Rex, so that machine does not need Tailscale installed. [CLI](https://www.superlogical.com/rex/docs/automate/cli), [Remote access](https://www.superlogical.com/rex/docs/linux/remote-access) | `phux host add` uses SSH you already have, then attach goes over QUIC. `phux host discover` reads the Tailscale client on this machine and lists the tailnet. Tailscale is optional. [Remote machines](./remote.md) |
| Who the server runs as | A personal server runs as you. A shared server runs as the `rex` account. A login server runs as root, opens a PAM login session, and starts a Rex server as that person's account. [Server modes](https://www.superlogical.com/rex/docs/linux/server-modes), [Login server](https://www.superlogical.com/rex/docs/linux/login-server) | One server per user per profile. The server never switches users, so every pane runs as that user. [Decisions](./decisions.md), [Server](./concepts.md#server), [`phux whoami`](./reference/cli.md#phux-whoami) |
| Sessions | The server keeps a session running whether or not a client is looking at it. A session holds windows (tabs). A window holds blocks. Today a block is a terminal. [CLI](https://www.superlogical.com/rex/docs/automate/cli) | Detach leaves programs running. When the server stops, those programs stop. The next server brings sessions, tabs, and panes back, without the programs and without scrollback. [What survives what](./concepts.md#what-survives-what) |
| A program's own status | A program can report idle, working, blocked, done, or error with the Program Status Protocol. Rex shows that in the session picker and on unfocused tab headers, and Lua can hear `terminal.program_status_changed`. [Program status](https://www.superlogical.com/rex/docs/build/program-status) | Agent state is evidence-ranked; screen is the floor. [Agent state](./building.md#agent-state), [Coding agents](./agents.md#agent-states) |
| Agents | `rex skill setup` installs a skill for Claude Code, Codex, OpenCode, and Pi, and a hook for Claude Code and Codex. [Agents](https://www.superlogical.com/rex/docs/automate/agent-skill) | Install paths for Claude Code, pi, Oh My Pi, OpenCode, Grok, and any MCP client. [Agent setup](./agents.md#agent-setup) |

The [homepage](https://www.superlogical.com/) plan says: "Sessions can be accessed through the web and native macOS/iOS applications, and sharing a live session with other people is built in from the start." The installation page says the iOS client is not in the public test yet.

`rex run` opens a terminal in a new window. `phux run` types a command into a pane that is already there and exits with that command's status. [Shell scripting](https://www.superlogical.com/rex/docs/automate/shell-scripting), [Scripting](./scripting.md).

Rex's shell scripting page says `rex capture` prints a terminal's screen and its scrollback, and that a terminal keeps about 64 MiB of scrollback. `phux snapshot` prints the screen; `--tail` includes history. [Shell scripting](https://www.superlogical.com/rex/docs/automate/shell-scripting), [Scripting](./scripting.md#reading-a-screen).

## When Rex is the better pick

Rex is the better pick when the job is one its own pages describe.

You want the macOS app to be the terminal, with its own local server in the app. The announcement calls that app a drop-in replacement for the terminal you use today. phux's core client is a terminal UI that runs inside whatever terminal you already have. The desktop app is a separate, newer client. [Clients](./clients.md).

You need a FreeBSD server. Rex documents one, and the installation page includes it in the current test. phux's install requirements list macOS Apple silicon and Linux with glibc. They do not list FreeBSD.

Several people use one Linux or FreeBSD machine, each as their own operating system account. Rex's login server is that mode: it identifies the caller, checks which accounts that caller may use, opens a PAM login session, and starts a Rex server as that account. phux does not: one server per user, and it never switches users. Rex says the login server has no passwords or keys yet. It identifies someone by how they connect, such as a Tailscale login, and it recommends Tailscale until the authentication it has planned is there. [Login server](https://www.superlogical.com/rex/docs/linux/login-server).

You want Lua for keys, key modes, and custom actions, and `rex do` to run an action by name. An action such as `pane.split` is performed by the app, so the app has to be running with Remote Control turned on. [Custom actions](https://www.superlogical.com/rex/docs/customize/actions), [Shell scripting](https://www.superlogical.com/rex/docs/automate/shell-scripting).

You want `rex skill setup` to install Rex's agent skill, including the hook that tells Claude Code or Codex which session and terminal it is in. [Agents](https://www.superlogical.com/rex/docs/automate/agent-skill).

You want a Linux or FreeBSD server on a tailnet without installing Tailscale on that machine. `rex server start --tailscale` uses the Tailscale built into Rex. Rex says that, today, it trusts a caller based only on how they connect. [Remote access](https://www.superlogical.com/rex/docs/linux/remote-access).

## When phux is the better pick

You want a public download today on macOS Apple silicon or Linux, from the [install script](https://phux.sh/install) or Homebrew, without joining a tester list. Rex's installation page still speaks to testers. The 5 October announcement said invites go to the mailing list, and that a download without contact information waits for the stable release.

You work on Windows. phux's CLI and terminal UI run in WSL 2, and the native desktop uses that server. Rex's installation page says its Windows server and its Windows client are not in the public test.

You already have SSH to the other machine. `phux host add` sets that machine up over SSH, then attach goes straight to its server over QUIC. A remote connection has to present the credentials from that pairing. Rex's remote listeners are a different set: built-in Tailscale, Tailscale Serve, a tailnet address, or plain HTTP if you pass `--allow-anonymous`. [Remote machines](./remote.md), [Remote access](https://www.superlogical.com/rex/docs/linux/remote-access).

You want the source, under Apache-2.0, in this repository. Rex's public docs do not state a software license, and on 2026-11-02 there was no public `LICENSE` file to check.

You drive panes from Claude Code, pi, Oh My Pi, OpenCode, Grok, or any MCP client, with `phux mcp` or the install line on the [agent setup](./agents.md#agent-setup) page. Rex's setup page names Claude Code, Codex, OpenCode, and Pi.

You want each pane's agent state recorded as evidence-ranked; screen is the floor, and `phux agent explain` prints the evidence for one pane. [Coding agents](./agents.md#agent-states).

## Status reports

Rex publishes the Program Status Protocol and shows a program's report in the session picker and on unfocused tab headers. phux reads that sequence too. For phux, agent state is evidence-ranked; screen is the floor. This page does not rank the two.

## Sources

Rex, 2026-11-02:

- [Homepage](https://www.superlogical.com/)
- [Public testing is beginning](https://www.superlogical.com/updates/public-testing-beginning) (5 October 2026)
- [Installation](https://www.superlogical.com/rex/docs/installation)
- [Installing on FreeBSD](https://www.superlogical.com/rex/docs/freebsd/installation)
- [Configuration](https://www.superlogical.com/rex/docs/customize/config)
- [Custom actions](https://www.superlogical.com/rex/docs/customize/actions)
- [The CLI](https://www.superlogical.com/rex/docs/automate/cli)
- [Shell scripting](https://www.superlogical.com/rex/docs/automate/shell-scripting)
- [Agents](https://www.superlogical.com/rex/docs/automate/agent-skill)
- [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status)
- [Privacy](https://www.superlogical.com/rex/docs/reference/privacy)
- [Server modes](https://www.superlogical.com/rex/docs/linux/server-modes)
- [Login server](https://www.superlogical.com/rex/docs/linux/login-server)
- [Remote access](https://www.superlogical.com/rex/docs/linux/remote-access)
- [superlogical on GitHub](https://github.com/superlogical), which listed no public repositories that day

phux, this repository the same day: [README](../README.md), [LICENSE](../LICENSE), and the docs linked in the table.
