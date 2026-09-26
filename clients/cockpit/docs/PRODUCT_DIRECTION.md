# Product Direction

## Thesis

Phux Cockpit is a native command environment for directed machine work.

The immediate application is a world-class native terminal for Phux and
terminal-based agents. The long-term product lets one person direct hundreds or
thousands of concurrent agents, runs, processes, and services without carrying
their individual state in their head. Repo positioning for that shape is
[ADR-0132](../../../docs/adr/0132-swarm-members-are-coordinator-clients.md):
swarm members are coordinator clients; Cockpit is a projection.

These are one path. Terminal quality proves the execution, identity, focus,
inspection, and intervention primitives that larger orchestration requires.

## The Wedge

The first product must be worth using before fleet orchestration exists:

- Immediate launch, input, rendering, scrolling, resizing, and focus
- Beautiful native tabs, splits, windows, menus, search, and accessibility
- Durable terminal identity independent from where it is displayed
- Clear process lifecycle, failure, recovery, and delivery state
- Excellent TUI-agent workflows with direct access to evidence and artifacts
- Local-first behavior that does not depend on a cloud control plane

The ambition is not to wrap another terminal. Cockpit should become the native
home for Phux work; `libghostty-vt` remains the terminal engine beneath it.

## The Larger Model

The fundamental product object is work, not a terminal, chat, or agent.

- **Objective:** an outcome being pursued
- **Work graph:** work units, dependencies, checkpoints, and decisions
- **Formation:** reusable roles, capabilities, policies, and escalation rules
- **Actor:** an agent, human, service, script, or persistent worker
- **Run:** one execution attempt by an actor
- **Session:** live interactive access to an actor or process
- **Artifact:** code, document, build, deployment, report, or other output
- **Signal:** progress, failure, risk, approval request, anomaly, or completion

Software construction, operations, research, support, and deployed-agent use
share this model. They differ in policy and latency, not architecture.

## Native Interaction

Cockpit should feel like a spatial operating environment, not a web admin
dashboard embedded in a shell.

- **Command:** state intent, navigate, allocate, approve, pause, and redirect
- **Map:** understand objectives, dependencies, formations, and critical paths
- **Attention:** see only decisions, blockers, risks, and abnormal outcomes
- **Workspace:** use a terminal, diff, artifact, document, browser, or live run
- **Inspector:** examine identity, lineage, policy, budget, and execution detail

At small scale, people manipulate individual sessions and runs directly. At
fleet scale, the same environment aggregates them into formations and outcomes.
Detail appears through progressive disclosure rather than a separate enterprise
mode.

Native means real system behavior: windows, focus, keyboard commands,
drag-and-drop, notifications, search, restoration, accessibility, and low
latency. Seamless means moving from objective to evidence to live intervention
without changing tools or reconstructing context.

## System Shape

The architecture should retain four boundaries:

1. **Cockpit:** native human command surface and projections
2. **Phux coordinator:** work identity, policy, scheduling, and durable state
3. **Providers and runners:** terminals, agents, processes, machines, services
4. **Event and artifact plane:** evidence, logs, outputs, checkpoints, lineage

Provider resources attach to stable product identities. Placement in a tab,
split, or window changes presentation, not execution identity. Local and remote
execution use the same product model even when their transport and durability
semantics differ.

## Sequence

1. Make the native Phux terminal exceptional for current terminal and TUI-agent
   work.
2. Prove stable provider identity, attachment, lifecycle, and evidence access.
3. Add one native work-graph slice: objective, dependent work, runs, exceptions,
   and artifact drill-down.
4. Prove that one person can comfortably direct 20-50 active workers.
5. Add reusable formations, policy, budgets, and distributed runners.
6. Scale the same model toward fleet operation without multiplying operator
   attention.

Detachable-window polish, generic dashboards, chat-first navigation, broad
provider abstractions, and persistence of live runtime memory are not shortcuts
to this sequence.

## Decision Test

For every product change, ask:

> Does this improve the terminal today and reduce the number of independent
> things the operator must mentally track tomorrow?

If it does neither, it is outside the product direction.
