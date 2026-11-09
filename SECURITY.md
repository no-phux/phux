# Security policy

## Supported versions

| Version | Supported |
| --- | --- |
| Latest stable release (`vX.Y.Z`, GitHub's Latest) | yes |
| Current LTS release (what `channels/lts.json` names) | yes, security fixes for 12 months from promotion |
| `next` builds | best effort; fixes land on `main` first |
| Anything older | no; update with `phux update` |

How the LTS line is promoted and patched is in
[docs/runbooks/lts.md](./docs/runbooks/lts.md).

## Reporting a vulnerability

Please do not open a public issue. Report privately through either:

- GitHub private vulnerability reporting:
  [Report a vulnerability](https://github.com/no-phux/phux/security/advisories/new)
- Email: security@phux.sh

Include the version (`phux --version`), the platform, and steps or a proof of
concept. We acknowledge reports within 5 business days.

## Coordinated disclosure

We ask for 90 days from your report to ship a fix before you disclose it
publicly, and we will agree an earlier date once a fixed release is out. We
credit reporters in the advisory unless you ask us not to.

## Scope

In scope:

- the server (`phux server`) and its local, WebSocket and QUIC listeners
- the relay (`phux-relay`)
- clients: the `phux` CLI and TUI, `phux-mcp`, Phux Rune, and the mobile
  client libraries
- the install script (`https://phux.sh/install`, `scripts/install.sh`) and
  `phux update`, including release signature and channel manifest checks

Out of scope: vulnerabilities in third-party agents or programs you run inside
a pane, and issues that need an attacker who already runs code as your user.
