---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-10-07
---

# Defguard-backed self-hosted federation masterplan

**TL;DR.** Use Defguard to operate a private routed WireGuard network and
phux to federate terminals over it. Explicit tunnel-address selection is
implemented; a real Defguard deployment is not validated here. Keep phux
TLS pins, per-link credentials, and authorization independent of VPN
membership. Start with a disposable two-host Linux pilot, then validate
mobile and unattended lifecycles before production rollout.

## Layering and topology

This plan selects Defguard as an independently deployed network service,
not a library embedded in phux. It follows the existing
[overlay boundary](../adr/0037-overlay-network-reachability.md),
[hub authentication](../adr/0038-hub-satellite-auth.md), and
[workload policy](../spec/workload-auth.md). No new protocol, network-provider
trait, Defguard API client, account system, or WireGuard implementation is
needed in phux.

There are two different hubs:

- The **Defguard gateway** forwards VPN packets. Core manages identities,
  devices and location configuration; Edge handles public enrollment and
  authentication flows. Core and PostgreSQL stay private.
- The **phux hub** routes terminal operations to registered satellites.
  It holds each satellite's phux credentials and certificate pin. It is
  trusted with the terminal traffic it relays; VPN encryption does not
  make it an end-to-end opaque router.

```text
operator laptop / phone --- WireGuard ---> Defguard gateway
                                               |
                       configured forwarding and return routes
                                               |
                    phux hub ---- pinned QUIC ---- phux satellite(s)
                        |                              |
                    local PTYs                     remote PTYs
```

Each phux node is a VPN peer or lives in a deliberately routed private
subnet. The phux hub can be a separate host from the gateway. Direct client
access to a satellite is optional; allow only the paths the deployment
actually needs. phux remains one server per OS user, not a shared multi-user
shell daemon. Different users on one machine need distinct listener ports.

Defguard documents gateway-backed **locations**, not an automatic peer mesh.
Client-to-client forwarding, Allowed IPs, ACLs and return paths must be
proven. Multiple locations do not establish inter-location routing.
Site-to-site LAN routing, automatic hole punching and a Defguard relay
fallback are not assumed. A gateway behind CGNAT needs a reachable placement
or explicit UDP forwarding; phux cannot supply a missing VPN route.

## Deployment baseline

Use the self-hosted open-core path first, with OS firewalls rather than a
paid API/ACL dependency. Pin compatible Core, Edge, Gateway and client/CLI
releases before writing deployment manifests. Record versions, image digests,
signature/checksum verification and the chosen license terms in the pilot
report. Do not copy an upstream demo installer into a production runbook.

Keep two enrollment classes separate:

- **Humans:** interactive Defguard desktop/mobile enrollment and MFA where
  supported by the selected release. Their VPN session can expire or need
  reauthorization; phux reconnect is not an MFA bypass.
- **Machines:** explicitly authorized unattended tunnels for federation
  hosts. Defguard documents Linux network devices and the `dg` service.
  A generic WireGuard configuration applies only to a non-MFA location.
  Network-device enrollment is not evidence of site-to-site LAN routing.
  Test unattended restart and revocation rather than scripting a human MFA
  session. macOS unattended parity is not established by the Linux guide.

Defguard's open core is AGPL; Enterprise code has separate terms. Current
pricing/license documentation gates managed firewall/ACLs, REST API and
some configuration-sync features behind paid plans, and Service Locations
and HA behind Enterprise. Service Locations are documented for Windows and
Linux and cannot use MFA or posture checks. The baseline does not depend
on them. Manual re-enrollment may be needed without automatic config sync.
Subscription renewal can require the vendor licensing server; self-hosting
alone does not prove offline independence. Recheck entitlements against the
pinned release; this is not a license determination or permission to bundle
Defguard code into phux.

## phux preparation

The shared `phux_config::overlay::detect` source now accepts
`PHUX_OVERLAY_ADDRS` for explicit provider-neutral address selection. Its
precedence, validation, profile gates and startup-only behavior live in
[operations](../operations.md#connecting-from-another-network-overlay-reachability).
[Remote access](../remote-access.md#defguard-managed-wireguard) owns the
commands; do not configure the same endpoint differently in this plan.

For long-lived nodes, prefer concrete `--quic` and `--listen` addresses
in the service unit. The environment override is useful for pairing,
doctor and overlay auto-listen; it does not install the VPN or make an IP
reachable. An environment export in an SSH shell is not automatically
inherited by a systemd/launchd service. Bring the machine tunnel up before
phux binds. A missing or changed address requires explicit recovery; no
VPN interface watcher or automatic address rebind is implemented.

The existing registry and hub dialer already accept private IPv4/IPv6 QUIC
or WSS endpoints. For unattended federation use a separate owner-only token
file and pin for each link, never inline secrets in `config.toml`. Existing
SSH enrollment can be useful after the route works, but it changes service
configuration; it is not a VPN installer. In a disposable pilot use manual
registry entries and a foreground `server --hub` to avoid altering real
service units.

A VPN peer gets no extra phux authority. `[policy] mode = "paired"` requires
workload client certificates as well as outer bearer admission. Normal
remote enrollment supports those certificates; do not switch a satellite
or phone to paired policy until its actual client path has been tested with
that requirement. The current satellite registry carries a bearer and pin,
not a workload-certificate reference. An owner-controlled, transitional
bearer-only pilot is therefore not a least-privilege multi-tenant deployment.
Use dedicated OS accounts/hosts to bound that pilot's blast radius and record
this limitation before exposing it to other users. Workload client identity
on hub-to-satellite links is tracked work `phux-676m` and blocks the
production gate; VPN setup alone cannot close that gap.

## Network and secret boundaries

| Flow | Exposure policy |
|---|---|
| Operator/device to Edge HTTPS | Public HTTPS only where remote enrollment/MFA requires it |
| Operator/device to Gateway WireGuard | Only the configured public UDP VPN port |
| Core, PostgreSQL, component management gRPC | Private management segment; never public |
| phux hub to satellite QUIC | VPN/private-route sources only, UDP 8788 by default |
| phux client to WSS | VPN/private-route sources only, TCP 8787 by default; enable only if needed |
| phux local UDS | Existing owner-only local authority; no public forwarding |

Use non-overlapping VPN/LAN prefixes and explicitly configured forward and
return routes. Allowed IPs selects routes; it is not a replacement for
firewall authorization. Restrict the gateway and each phux host, including
when Defguard-managed ACLs are unavailable. Do not expose phux's listeners
on the gateway's public interface. A phux relay is not needed for this
baseline and would introduce another trusted traffic terminator.

Keep WireGuard keys, enrollment tokens, phux bearer files, workload private
keys and database credentials out of git, process arguments and collected
logs. Pairing links/QRs contain the phux bearer. Back up PostgreSQL and
Edge/Gateway certificate state separately, plus each phux server's retained
identity and credential stores. Test restore without making certificates
or tokens public. VPN-device revocation and phux-credential revocation are
separate operations; compromise recovery needs both.

## Delivery gates

Beads owns execution status and dependencies, not this document. The stages
below define acceptance, not a claim that deployment is complete.

### 1. Disposable routed pilot

Tracked work: `phux-za0v`.

1. Allocate an isolated Linux lab: private Core/DB, Edge, a reachable
   Gateway, one phux hub and one satellite. Record release/entitlement
   choices, IP allocation, routes and firewall rules. Never point a build
   at the installed phux socket, state directory or production profile.
2. Enroll both hosts as authorized machine peers. Prove hub-to-satellite
   VPN traffic and the return path, not merely a gateway handshake or ping.
3. Run phux from the build in isolated profiles, temp UDS/state/config
   directories and concrete test listener ports. Pair per-link credentials,
   register the satellite, and start the hub with `--hub`.
4. Exercise aggregate inventory, satellite spawn, `host/@N` snapshot/input,
   output streaming, detach and reconnect. Transfer large output to expose
   MTU/PMTU problems, not just small control messages. Existing federation
   integration tests are regression evidence, not a substitute for this lab.
5. Wrong pins, missing/wrong/revoked tokens and unauthorized VPN peers must
   fail. Disconnect/rejoin the VPN and restart the gateway: remote loss
   needs bounded diagnosis while local PTYs remain alive. Do not replay
   destructive operations speculatively during recovery.
6. Capture results without secrets and tear the lab down. Promote only with
   reproducible successful flows and explained negative/failure cases.

### 2. Platform and lifecycle acceptance

Tracked work: `phux-52qy`, dependent on the routed pilot.

Validate Linux machine boot without an interactive login, stale config and
revocation. Verify any macOS machine-tunnel choice separately. Test actual
phux-mobile over the official Defguard iOS/Android VPN clients: enrollment,
MFA reauthorization, split routes, background/sleep, Wi-Fi/cellular handoff,
QUIC and explicit WSS. A supported VPN app does not establish uninterrupted
background connectivity or mobile phux-server hosting. Record the supported
platform/release matrix, including gaps.

### 3. Production rollout and recovery

Tracked work: `phux-fusq`, dependent on both earlier gates and the
satellite workload-identity work `phux-676m`.

Use explicitly selected infrastructure and verified owner/admin authority.
Roll out one satellite before a fleet. Prove denied network paths, least-
privilege phux policy compatibility, credential rotation and immediate
revocation, gateway loss, Core/Edge loss, license-expiry behavior if paid
features are used, and a backup restore. Cached gateway configuration may
survive Core loss; that is not proof fresh enrollment or MFA still works.

Monitor tunnel health, route reachability, phux listener state and degraded
satellite status separately. Preserve a local UDS/console recovery path.
Rollback disables new satellite links and their VPN access, revokes their
phux credentials, and restores the previous service configuration without
deleting live terminal state. Production firewall/network changes and
release installation are not part of this preparation patch.

## Status

| Capability | Evidence or remaining gate |
|---|---|
| Explicit non-Tailscale address selection | Implemented in `phux-config`; parser and isolated CLI pairing regression tests |
| QUIC/WSS federation over a routable IP | Existing transport/registry/hub implementation; VPN-specific pilot still required |
| Real Defguard client-to-client routing | Not validated; tracked pilot `phux-za0v` |
| Unattended hosts and phone lifecycle | Not validated; tracked acceptance `phux-52qy` |
| Scoped multi-user satellite admission and production recovery | Not established by this patch; policy compatibility and rollout tracked in `phux-fusq` |

## Sources and next reading

Primary sources retrieved 2026-10-07; upstream documentation is unversioned
and some old links/desktop-only MFA claims differ from current mobile docs.
Revalidate against the selected release before deployment:

- [Defguard repository and license](https://github.com/DefGuard/defguard).
- [Architecture](https://docs.defguard.net/in-depth/architecture) and
  [production deployment verification](https://docs.defguard.net/deployment-strategies/production-deployment-verification-guide).
- [VPN locations](https://docs.defguard.net/features/wireguard/create-your-vpn-network)
  and [network devices](https://docs.defguard.net/features/network-devices).
- [Service Locations](https://docs.defguard.net/features/service-locations),
  [license/plan requirements](https://docs.defguard.net/enterprise/license),
  and [pricing](https://defguard.net/pricing/).
- [Mobile connection](https://docs.defguard.net/using-defguard-for-end-users/mobile-client/instance-connect)
  and [generic WireGuard clients](https://docs.defguard.net/using-defguard-for-end-users/adding-wireguard-devices).
- [phux remote access](../remote-access.md) and
  [operational trust model](../operations.md#remote-consumer-trust-model-opt-in).
