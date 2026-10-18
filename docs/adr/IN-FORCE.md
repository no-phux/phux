---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-18
---

# Decisions in force

**TL;DR.** Current decisions by topic, newest first; Proposed records remain
in the trailing block. Follow each record for its contract, rationale, and
relationships. The [numeric index](./README.md) is the other navigation view.

Topics include Accepted and Accepted (forward-compat) records. A newer
amending decision is operative where it overlaps an older one; the older
record retains its historical rationale. Update membership when a record's
status changes. The docs gate checks identities, ordering, and membership.

## Identity and kinds

- [0160](./0160-sessions-own-resources-and-window-is-retired.md) Sessions own resources; Window is retired
- [0145](./0145-the-tui-sizes-panes-and-casts-no-viewport-vote.md) The TUI sizes panes and casts no viewport vote
- [0126](./0126-idempotent-create.md) Idempotent create
- [0125](./0125-kind-catalog-is-generated-metadata.md) Kind catalog is generated metadata
- [0124](./0124-retain-on-exit.md) Retain on exit
- [0104](./0104-parent-bindings-are-l1-lifecycle.md) Parent bindings are L1 lifecycle
- [0102](./0102-resources-the-server-serves-kinds.md) Resources the server serves kinds
- [0064](./0064-playback-as-a-pane.md) Playback as a pane
- [0062](./0062-headless-resize-and-window-size-policy.md) Headless resize and window size policy
- [0056](./0056-cross-session-terminal-move.md) Cross-session Terminal move
- [0050](./0050-explicit-spawn-ownership.md) Explicit spawn ownership
- [0027](./0027-terminal-references-and-l3-links.md) Terminal references and L3 links
- [0015](./0015-protocol-layering.md) Protocol layering
- [0011](./0011-protocol-core-independence.md) Protocol core independence

## Wire and codecs

- [0161](./0161-organization-is-typed-commands-behind-a-feature-bit.md) Organization is typed commands behind a feature bit
- [0142](./0142-host-path-query-is-separate-from-directory-listing.md) Host path queries do not change directory listings

- [0137](./0137-server-feature-word-extends.md) Server feature word extends (forward-compat)
- [0123](./0123-events-are-journaled.md) Events are journaled
- [0086](./0086-shared-render-pool.md) Shared render pool
- [0117](./0117-wire-codec-stays-tlv.md) Wire codec stays TLV
- [0061](./0061-capabilities-add-versions-break.md) Capabilities add versions break
- [0060](./0060-self-contained-session-recording.md) Self-contained session recording
- [0059](./0059-sandboxed-chunked-file-upload.md) Sandboxed chunked file upload
- [0034](./0034-kitty-graphics-image-passthrough.md) Kitty graphics image passthrough
- [0024](./0024-wire-owns-input-atoms.md) Wire owns input atoms
- [0021](./0021-control-plane-commands.md) Control plane commands
- [0013](./0013-libghostty-bytes-on-wire.md) libghostty bytes on wire
- [0008](./0008-use-libghostty-types-directly.md) Use libghostty types directly
- [0006](./0006-input-mirrors-libghostty.md) Input mirrors libghostty

## Server process and actor model

- [0162](./0162-organization-records-persist-and-activations-reconcile.md) Organization records persist; activations reconcile
- [0150](./0150-the-server-keeps-the-restore-archive-current.md) The server keeps the restore archive current
- [0130](./0130-on-disk-pty-journal-is-not-built.md) On disk pty journal is not built
- [0114](./0114-cockpit-closes-terminals-and-detaches-windows.md) Cockpit closes terminals and detaches windows
- [0096](./0096-always-on-performance-telemetry.md) Always on performance telemetry
- [0088](./0088-adopting-a-live-server-into-supervision.md) Adopting a live server into supervision
- [0083](./0083-in-place-supervisor-unit-reconcile.md) In-place supervisor unit reconcile
- [0080](./0080-socket-lifecycle-and-instance-isolation.md) Socket lifecycle and instance isolation
- [0073](./0073-service-managed-pane-login-shell.md) Service managed pane login shell
- [0063](./0063-ephemeral-server-lifetime.md) Ephemeral server lifetime
- [0055](./0055-always-on-server-and-ssh-bootstrapped-enrollment.md) Always-on server and ssh-bootstrapped enrollment
- [0032](./0032-graceful-server-upgrade.md) Graceful server upgrade
- [0028](./0028-runtime-log-control.md) Runtime log control (forward-compat)
- [0014](./0014-server-terminal-pane-actor.md) Server terminal pane actor
- [0005](./0005-relationship-to-zmx-and-zmosh.md) Relationship to zmx and zmosh
- [0004](./0004-libghostty-vt-as-grid.md) libghostty VT as grid
- [0003](./0003-server-process-model.md) Server process model

## State sync and bootstrap

- [0143](./0143-ship-ten-mib-of-scrollback-per-pane.md) Ship 10 MiB of scrollback per pane
- [0119](./0119-attach-leases-retained-history.md) Attach leases retained history
- [0094](./0094-explicit-per-pane-scrollback-byte-ceiling.md) Explicit per pane scrollback byte ceiling
- [0090](./0090-confirmation-gated-predictive-echo.md) Confirmation gated predictive echo
- [0070](./0070-native-engine-state-bootstrap.md) Native engine state bootstrap
- [0043](./0043-state-diff-output-mode.md) State diff output mode
- [0018](./0018-lazy-state-synchronization.md) Lazy state synchronization

## Input authority

- [0144](./0144-input-credits-backpressure-instead-of-drop.md) Input credits backpressure instead of drop
- [0053](./0053-acknowledged-idempotent-input.md) Acknowledged idempotent input
- [0044](./0044-dedicated-input-lane.md) Dedicated input lane
- [0127](./0127-attach-roles-are-lease-intent.md) Attach roles are lease intent
- [0128](./0128-approvals-are-held-actions.md) Approvals are held actions
- [0033](./0033-input-authority-and-process-signals.md) Input authority and process signals

## Federation and transport

- [0185](./0185-quictun-gets-acme-certificates-and-multiplexes-consumers.md) quictun gets ACME certificates and multiplexes consumers
- [0183](./0183-quictun-is-a-standalone-tunnel-product.md) quictun is a standalone tunnel product
- [0182](./0182-satellites-enroll-and-dial-out-to-the-hub.md) Satellites enroll and dial out to the hub
- [0181](./0181-the-relay-is-a-standalone-quic-tunnel-crate.md) The relay is a standalone QUIC tunnel crate
- [0120](./0120-ssh-bootstrap-opens-a-listener-per-attach.md) Ssh bootstrap opens a listener per attach
- [0115](./0115-quic-stream-per-terminal.md) QUIC stream per terminal
- [0111](./0111-how-a-front-restore-is-judged.md) How a front restore is judged
- [0110](./0110-a-showing-peer-is-re-shown-at-launch-only-in-front.md) A showing peer is re-shown at launch only in front
- [0109](./0109-late-kills-are-conditional-on-instance-and-attachment.md) Late kills are conditional on instance and attachment
- [0136](./0136-hub-mirrors-satellite-agent-metadata.md) Hub mirrors satellite agent metadata
- [0108](./0108-a-hub-relays-host-queries-per-request.md) A hub relays host queries per request
- [0107](./0107-satellite-sessions-are-listed-never-adopted.md) Satellite sessions are listed never adopted
- [0141](./0141-pair-mints-only-against-a-live-listener.md) Pair mints only against a live listener
- [0164](./0164-pair-links-name-the-magicdns-host.md) Pair links name the magicdns host
- [0122](./0122-host-add-is-the-front-door.md) Host add is the front door
- [0093](./0093-remote-target-as-a-resolution-ladder.md) Remote target as a resolution ladder
- [0081](./0081-overlay-auto-listen-and-one-command-pairing.md) Overlay auto-listen and one-command pairing
- [0066](./0066-host-namespace.md) Host namespace
- [0149](./0149-relay-routes-ride-the-tls-server-name-everywhere.md) Relay routes ride the TLS server name everywhere
- [0057](./0057-minimal-reference-relay.md) Minimal reference relay
- [0052](./0052-connector-route-identity-and-config.md) Connector route identity and config
- [0051](./0051-outbound-dial-out-connector-transport.md) Outbound dial out connector transport
- [0037](./0037-overlay-network-reachability.md) Overlay network reachability (forward-compat)
- [0025](./0025-browser-web-client.md) Browser web client
- [0007](./0007-mosh-class-transport-and-satellites.md) Mosh class transport and satellites (forward-compat)

## Auth and trust

- [0180](./0180-the-relay-is-blind.md) The relay is blind
- [0179](./0179-the-legacy-auth-paths-are-removed.md) The legacy auth paths are removed
- [0163](./0163-actors-and-delegated-agent-grants.md) Actors and delegated agent grants
- [0154](./0154-devices-enroll-with-a-ticket-over-their-own-alpn.md) Devices enroll with a ticket over their own alpn
- [0153](./0153-clients-pin-the-workload-ca.md) Clients pin the workload ca
- [0116](./0116-workload-auth-is-mtls.md) Workload auth is mTLS
- [0106](./0106-identity-is-the-serving-user.md) Identity is the serving user
- [0098](./0098-workload-proof-and-closed-scope-authority.md) Workload proof and closed-scope authority (forward-compat)
- [0091](./0091-certificate-names-the-advertised-address.md) Certificate names the advertised address
- [0038](./0038-hub-satellite-auth.md) Hub satellite auth
- [0031](./0031-remote-consumer-auth-and-encryption.md) Remote consumer auth and encryption

## Agents

- [0187](./0187-agents-coordinate-through-phux.md) Agents coordinate through phux
- [0159](./0159-agent-streams-resume-from-committed-records.md) Agent streams resume from committed records
- [0156](./0156-agent-transcript-records.md) Agent transcript records
- [0155](./0155-host-originated-push-through-a-blind-gateway.md) Host originated push through a blind gateway
- [0151](./0151-live-agent-sessions-bridge-into-native-restore.md) Live agent sessions bridge into native restore
- [0118](./0118-agent-integrations-share-a-neutral-runtime.md) Agent integrations share a neutral runtime
- [0103](./0103-agent-session-resource-and-producer-fed-streams.md) Agent session resource and producer-fed streams
- [0097](./0097-durable-coordinator-is-a-separate-bounded-endpoint.md) Durable coordinator is a separate bounded endpoint (forward-compat)
- [0085](./0085-hook-sourced-agent-state.md) Hook sourced agent state
- [0084](./0084-starting-an-agent-in-an-existing-shell.md) Starting an agent in an existing shell
- [0078](./0078-alternate-screen-history.md) Alternate screen history (forward-compat)
- [0077](./0077-agent-read-surface.md) Agent read surface
- [0076](./0076-agent-prompt-and-lifecycle-wait.md) Agent prompt and lifecycle wait
- [0075](./0075-agent-name-addressing.md) Agent name addressing
- [0068](./0068-native-agent-session-restore.md) Native agent-session restore
- [0067](./0067-cache-preserving-agent-fleet-context.md) Cache-preserving agent fleet context
- [0046](./0046-server-side-agent-state-detection.md) Server side agent state detection
- [0042](./0042-launch-executor.md) Launch executor
- [0040](./0040-agent-identity-metadata.md) Agent identity metadata
- [0036](./0036-agent-asked-detection.md) Agent asked detection
- [0035](./0035-agent-asked-event.md) Agent asked event
- [0022](./0022-tool-for-agents.md) Tool for agents
- [0009](./0009-phux-vs-mux-positioning.md) phux vs mux positioning

## TUI conventions

- [0172](./0172-tui-actions-and-jobs-are-declared-once.md) TUI actions and jobs are declared once
- [0148](./0148-plugin-sidebar-sections-are-fixed-bands-of-pane-rows.md) Plugin sidebar sections are fixed bands of pane rows
- [0147](./0147-plugin-overlay-panes-float-outside-the-layout.md) Plugin overlay panes float outside the layout
- [0146](./0146-tui-pushes-kitty-keyboard-disambiguate-on-the-host.md) TUI pushes kitty keyboard disambiguate on the host
- [0140](./0140-sidebar-machines-come-from-a-hosts-provider.md) Sidebar machines come from a hosts provider
- [0165](./0165-gpui-desktop-replaces-the-solid-host.md) GPUI desktop replaces the Solid host
- [0135](./0135-one-binding-crate.md) One binding crate
- [0134](./0134-connected-lanes-refine-the-binding-boundary.md) Connected lanes refine the binding boundary
- [0171](./0171-one-transport-phux-client-rides-the-runtime.md) One transport phux client rides the runtime
- [0133](./0133-one-client-runtime-below-every-binding.md) One client runtime below every binding
- [0129](./0129-projections-are-named-by-key.md) Projections are named by key
- [0100](./0100-the-tui-is-its-own-crate.md) The TUI is its own crate
- [0089](./0089-three-zone-attention-sidebar.md) Three zone attention sidebar
- [0112](./0112-stable-split-sidebar-navigation.md) Stable split sidebar navigation
- [0079](./0079-fatal-signal-terminal-restore.md) Fatal-signal terminal restore
- [0121](./0121-cli-parser-is-usage-rs.md) CLI parser is usage rs
- [0065](./0065-one-cli-grammar.md) One CLI grammar
- [0058](./0058-right-click-context-menus.md) Right-click context menus
- [0174](./0174-organize-asks-a-judge-and-code-decides.md) Organize asks a judge; code decides
- [0168](./0168-project-identity-is-a-session-attribute.md) Project identity is a session attribute
- [0152](./0152-project-recipes-run-only-approved-bytes.md) Project recipes run only approved bytes
- [0054](./0054-worktree-bound-sessions.md) Worktree bound sessions
- [0049](./0049-client-local-focus-and-advisory-attention.md) Client local focus and advisory attention
- [0048](./0048-drag-to-resize-and-default-mouse-capture.md) Drag to resize and default mouse capture
- [0045](./0045-client-side-copy-mode.md) Client side copy mode
- [0158](./0158-pane-clipboard-writes-are-client-policy.md) Pane clipboard writes are client policy
- [0029](./0029-one-cursor-authority-and-repaint-scheduler.md) One cursor authority and repaint scheduler
- [0026](./0026-overlays-theme-stack-single-dispatch.md) Overlays theme stack single dispatch
- [0020](./0020-layered-render.md) Layered render
- [0019](./0019-tui-multi-pane-rendering.md) TUI multi pane rendering
- [0017](./0017-tui-not-protocol-privileged.md) TUI not protocol privileged
- [0012](./0012-binary-split-tree-layout.md) Binary split tree layout
- [0010](./0010-frontend-agnostic-tmux-cc-reserved.md) Frontend agnostic tmux cc reserved (forward-compat)

## Config

- [0186](./0186-the-client-neutral-config-is-its-own-crate.md) The client-neutral config is its own crate
- [0101](./0101-the-settings-page-edits-the-file.md) The settings page edits the file
- [0041](./0041-managed-plugin-installs.md) Managed plugin installs
- [0039](./0039-layered-config.md) Layered config
- [0023](./0023-config-ux-philosophy.md) Config ux philosophy

## Release, CI, and docs

- [0184](./0184-build-and-ci-logic-is-one-bun-package.md) Build and CI logic is one Bun package
- [0099](./0099-ci-aggregate-gate-and-action-supply-chain.md) Ci aggregate gate and action supply chain
- [0082](./0082-retire-the-ci-metrics-store.md) Retire the ci metrics store
- [0178](./0178-release-checksums-are-minisign-signed.md) Release checksums are minisign-signed
- [0176](./0176-the-release-pipeline.md) The release pipeline
- [0175](./0175-one-release-manifest-per-channel.md) One release manifest per channel
- [0169](./0169-zig-cockpit-lives-in-an-archive.md) Zig cockpit lives in an archive
- [0074](./0074-self-update-trust-boundary.md) Self update trust boundary
- [0069](./0069-generated-reference-docs.md) Generated reference docs
- [0001](./0001-language-rust.md) Use Rust

- [0170](./0170-acp-is-the-provider-seam.md) Acp is the provider seam

## Proposed

- [0188](./0188-quick-capture.md) Quick capture

Drafted and under review; none of these governs anything yet.

- [0177](./0177-non-terminal-resources-have-declared-producers.md) Non terminal resources have declared producers
- [0173](./0173-mission-control-is-a-client-composition.md) Mission control is a client composition
- [0166](./0166-usage-collection-is-a-supervised-child.md) Usage collection is a supervised child
- [0157](./0157-themes-are-colors-toml-files-every-client-reads.md) Themes are colors toml files every client reads
- [0132](./0132-swarm-members-are-coordinator-clients.md) Swarm members are coordinator clients
- [0092](./0092-durable-work-coordinator-authority.md) The coordinator owns durable work
- [0087](./0087-elastic-status-bar-space.md) Elastic status bar space
- [0072](./0072-prune-policy-vocabulary-keep-the-seam.md) Prune policy vocabulary keep the seam
- [0071](./0071-what-phux-1-0-commits-to.md) What phux 1.0 commits to
