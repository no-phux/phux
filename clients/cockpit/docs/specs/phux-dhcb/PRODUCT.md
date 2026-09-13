---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-10
---

# Cockpit canvas: navigation and appearance

**TL;DR.** Establish a cohesive native canvas for existing Phux work: restrained
chrome, recognizable workspace navigation, explicit session and known-host
selection, and reversible appearance settings. This is the first vertical slice
of the approved precision-instrument direction, not the entire spatial roadmap.

## Summary

Host selection, workspace organization, and settings receive the same visual and
interaction care as the terminal. People can orient themselves, switch work,
organize their current view, and experiment with appearance without losing focus
or confusing placement with execution.

## Design context

The user approved the conversational precision-instrument proposal and requested
a bounded vertical slice with particular care for hosts, workspaces and settings.
Figma: none provided. The existing Geist tokens and 4pt chrome register govern
the implementation; native layout and live inspection are the design medium.

## Behavior

1. The titlebar, navigation and terminal form one composition. Selected tabs use
   a quiet surface and a clear accent indicator; the terminal remains visually
   dominant. Healthy connection status does not require a full-width footer.
2. The current session/coordinator context is visible and opens workspace
   navigation. Connection failures and terminal recovery remain visible and
   distinguishable from normal state. Endpoint text describes the real connection.
3. Navigation supports all work, sessions, and known terminal hosts. Host choices
   are inferred only from known terminal resources and never claim exhaustive
   host discovery, online status or provisioning. Selecting a host filters
   terminals by that exact host identity, not by a substring in their title.
4. Results distinguish already-open panes, available terminals and sessions.
   They show useful title and location context. Search matches catalog titles,
   directories and host names even when a terminal has no live local view.
5. Choosing an open pane focuses its exact native window/tab/pane. Choosing a
   session uses the existing Phux session-switch behavior. Unavailable resource
   ownership does not silently create replacement execution.
6. Loading, empty, offline and stale results have explicit states. Filtering and
   pagination never let an old result act on a new resource occupying its index.
   Escape dismisses; keyboard navigation and Enter remain fast and predictable.
7. Top tabs and the workspace rail are alternative placements with equivalent
   navigation. The rail distinguishes current tabs and their agent summaries.
   Secondary windows receive the same canvas treatment. Overflow is actionable.
8. Appearance settings has a deliberate, readable page with grouped controls,
   live theme preview, terminal text size, local cursor defaults and workspace placement.
   Connection information and configuration location have their own section.
9. Appearance experiments are a transaction. Opening captures the starting
   appearance. Preview never writes disk. Cancel restores it, including unnamed
   or system-following themes and explicit overrides. Save writes only the
   supported settings actually changed, preserving unrelated lines and comments.
10. A save failure keeps the page open and explains that the preview is not
    saved. Cancel remains available. A missing configuration file can be created;
    unreadable, oversized or malformed destination state is never overwritten as
    though it were an empty file.
11. Explicit foreground/background overrides retain precedence over theme
    choices and are explained. The page itself stays readable independent of the
    preview. Unsupported capabilities do not appear as functioning controls.
12. Exactly one surface visibly owns keyboard input. The settings page is
    presented as a modal editing surface in this slice; closing it restores
    terminal interaction. Focus, selection and process identity survive layout
    and theme changes. Terminal content and viewport geometry are never animated.

## Scope

This slice establishes the shared canvas and existing provider-backed navigation.
Cross-window drag transfer, fleet overview, arbitrary endpoint switching, host
enrollment and a complete command catalogue remain later product work. The
settings page uses the existing native canvas/modal lifecycle rather than
introducing another window beyond the SDK's bounded terminal-window slots.
