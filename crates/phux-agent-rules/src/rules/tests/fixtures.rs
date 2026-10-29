    // --- Shipped manifests against captured screens ------------------------
    //
    // The fixtures are REAL viewports (`phux snapshot --json`) and titles
    // captured from each CLI. Synthetic screens only test the matcher against
    // itself; re-capture when an agent's TUI changes, never hand-edit. The
    // gemini, goose, and aider captures drove the real CLI against a localhost
    // model stand-in, so their transcript text is synthetic and their chrome
    // is not; each manifest header records how.

    /// Claude titles: an animated prefix while busy (braille in 2.1.207,
    /// half circles in 2.1.228), a static U+2733 otherwise. The quiet title
    /// covers idle AND a permission dialog, so it must assert nothing.
    const CLAUDE_BUSY: [&str; 4] = [
        "\u{2802} phux",
        "\u{2810} phux",
        "\u{25D0} phux",
        "\u{25D1} phux",
    ];
    const CLAUDE_QUIET: &str = "\u{2733} phux";

    /// Every OMP title spinner frame: braille (default), dots, pulse, line,
    /// and the static WSL/Windows separator.
    const OMP_WORKING_SEPARATORS: [&str; 26] = [
        "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}",
        "\u{2827}", "\u{2807}", "\u{280f}", "\u{2801}", "\u{2802}", "\u{2804}", "\u{2820}",
        "\u{2810}", "\u{2808}", "\u{25cb}", "\u{25d4}", "\u{25d1}", "\u{25d5}", "\u{25cf}", "-",
        "\\", "|", "/", ":",
    ];

    /// `kind | title | progress | screen | want`, one golden case per line.
    /// `screen` is a fixture path or an `@name` from [`named_screen`]; `want`
    /// is `<state> <rule>`, `nothing` (the fail-safe decides), `not-blocked`,
    /// or `freeze`. A false `blocked` destroys trust in the feature, so
    /// transcript-quoted dialogs are pinned `not-blocked`.
    const GOLDEN: &str = "
claude |  | 4;3; | claude/idle_prompt.txt | working osc-progress-working
claude |  | 4;0; | claude/idle_prompt.txt | idle osc-progress-idle
claude |  | 4;3; | claude/blocked_permission.txt | blocked prompt-permission-dialog
claude | \u{2733} phux |  | claude/blocked_permission.txt | blocked prompt-permission-dialog
claude |  |  | claude/blocked_permission.txt | blocked prompt-permission-dialog
claude | \u{2733} phux |  | claude/idle_prompt.txt | nothing
claude | \u{2802} phux |  | claude/working.txt | working title-busy-spinner
claude |  |  | claude/working.txt | working screen-status-elapsed-backstop
claude | \u{2733} phux |  | @claude-transcript-dialog | not-blocked
claude | \u{2733} phux |  | @bare-dialog | not-blocked
claude | \u{2733} phux |  | @claude-pager | freeze
codex | tmp |  | codex/blocked_approval.txt | blocked prompt-command-approval
codex | tmp |  | codex/working.txt | working screen-working-footer-backstop
codex | tmp |  | codex/idle_prompt.txt | nothing
codex | tmp |  | @codex-prose | not-blocked
opencode | OC | whatever |  | opencode/working.txt | working footer-interrupt-affordance
opencode | OC | whatever |  | opencode/blocked_permission.txt | blocked permission-required-dialog
opencode | OpenCode |  | opencode/idle_prompt.txt | nothing
opencode | OC | esc interrupt |  | opencode/idle_prompt.txt | nothing
grok |  |  | grok/working.txt | working screen-status-elapsed-backstop
grok | grok |  | grok/blocked_trust.txt | blocked folder-trust-dialog
grok | grok |  | grok/idle_prompt.txt | nothing
grok | grok |  | @grok-quoted-dialog | not-blocked
amp | \u{280a} Terminal haiku - amp - /tmp/ws |  | @amp-idle | working title-busy-spinner
amp |  |  | @amp-working | working prompt-box-activity-footer
amp | Terminal haiku - amp - /tmp/ws |  | @amp-idle | nothing
cursor-agent |  |  | @cursor-login | nothing
gemini | \u{2726}  Working\u{2026} (work) |  | gemini/working.txt | working title-working
gemini | \u{2726}  Working\u{2026} (work) |  | gemini/working_22s.txt | working title-working
gemini | \u{2726}  Working\u{2026} (work) |  | gemini/working_1m.txt | working title-working
gemini | \u{270b}  Action Required (work) |  | gemini/blocked_shell_approval.txt | blocked title-action-required
gemini | \u{25c7}  Ready (work) |  | gemini/idle_prompt.txt | nothing
gemini | \u{25c7}  Ready (work) |  | gemini/blocked_trust.txt | blocked folder-trust-dialog
gemini | \u{25c7}  Ready (work) |  | gemini/auth_dialog.txt | nothing
gemini | Gemini CLI (work) |  | gemini/working_static_title.txt | working screen-esc-to-cancel
gemini | Gemini CLI (work) |  | gemini/blocked_static_title.txt | blocked tool-approval-dialog
gemini |  |  | gemini/working.txt | working screen-esc-to-cancel
gemini |  |  | gemini/working_22s.txt | working screen-esc-to-cancel
gemini |  |  | gemini/working_1m.txt | working screen-esc-to-cancel
gemini |  |  | gemini/blocked_shell_approval.txt | blocked tool-approval-dialog
gemini |  |  | gemini/blocked_trust.txt | blocked folder-trust-dialog
gemini |  |  | gemini/idle_prompt.txt | nothing
gemini |  |  | gemini/auth_dialog.txt | nothing
gemini | Gemini CLI (work) |  | @gemini-quoted-dialogs | not-blocked
goose | \u{1fabf} gwork |  | goose/working.txt | working spinner-interrupt-hint
goose | \u{1fabf} gwork |  | goose/working_8s.txt | working spinner-interrupt-hint
goose | \u{1fabf} gwork |  | goose/working_14s.txt | working spinner-interrupt-hint
goose | \u{1fabf} gwork |  | goose/working_20s.txt | working spinner-interrupt-hint
goose | \u{1fabf} gwork |  | goose/blocked_tool_approval.txt | blocked tool-approval-prompt
goose | \u{1fabf} gwork |  | goose/idle_prompt.txt | nothing
goose | \u{1fabf} gwork |  | goose/idle_interrupted.txt | nothing
goose | \u{1fabf} gwork |  | goose/streaming_no_spinner.txt | nothing
goose | \u{1fabf} gwork |  | @goose-dialog-above-idle | not-blocked
aider |  |  | aider/working.txt | working waiting-for-model
aider |  |  | aider/working_8s.txt | working waiting-for-model
aider |  |  | aider/working_13s.txt | working waiting-for-model
aider |  |  | aider/working_18s.txt | working waiting-for-model
aider |  |  | aider/blocked_shell_confirm.txt | blocked confirm-prompt
aider |  |  | aider/idle_prompt.txt | nothing
aider |  |  | aider/idle_after_decline.txt | nothing
aider |  |  | aider/streaming_no_spinner.txt | nothing
aider |  |  | @aider-dialog-above-idle | not-blocked
pi |  |  | pi/idle_prompt.txt | nothing
pi |  |  | pi/working.txt | working bottom-working-status
pi |  |  | pi/blocked_trust.txt | blocked project-trust-dialog
pi |  |  | @pi-dialog-above-idle | not-blocked
omp |  |  | omp/idle_prompt.txt | nothing
omp |  |  | omp/working.txt | working bottom-running-status
omp |  |  | omp/blocked_tool_approval.txt | blocked tool-approval-dialog
omp |  |  | @omp-dialog-above-idle | not-blocked
omp | \u{3c0} > work |  | omp/v18_idle_new_session.txt | nothing
omp | \u{3c0} > Run sleep and echo finished |  | omp/v18_idle_prompt.txt | nothing
omp | \u{3c0} > Run sleep and echo finished |  | omp/v18_idle_interrupted.txt | nothing
omp | \u{3c0} \u{2807} work |  | omp/v18_working_thinking.txt | working title-working-spinner
omp | \u{3c0} \u{2839} Run sleep then echo finished |  | omp/v18_working.txt | working title-working-spinner
omp | \u{3c0} \u{280f} Run sleep and echo command |  | omp/v18_working_queued.txt | working title-working-spinner
omp | \u{3c0} ! Run sleep and echo finished |  | omp/v18_blocked_tool_approval.txt | blocked title-attention
omp | \u{3c0} ! Run sleep and echo finished |  | omp/v18_blocked_ask.txt | blocked title-attention
omp | \u{3c0} > Run sleep then echo finished |  | omp/v18_field_idle_prompt.txt | nothing
omp | \u{3c0} \u{2819} work |  | omp/v18_field_working_thinking.txt | working title-working-spinner
omp | \u{3c0} \u{2826} Run sleep and echo finished |  | omp/v18_field_working.txt | working title-working-spinner
omp | \u{3c0} ! Run sleep then echo finished |  | omp/v18_field_blocked_tool_approval.txt | blocked title-attention
omp |  |  | omp/v18_idle_new_session.txt | nothing
omp |  |  | omp/v18_idle_prompt.txt | nothing
omp |  |  | omp/v18_idle_interrupted.txt | nothing
omp |  |  | omp/v18_field_idle_prompt.txt | nothing
omp |  |  | omp/v18_working_thinking.txt | working status-spinner-elapsed
omp |  |  | omp/v18_working.txt | working status-spinner-elapsed
omp |  |  | omp/v18_working_queued.txt | working status-spinner-elapsed
omp |  |  | omp/v18_field_working_thinking.txt | working status-spinner-elapsed
omp | \u{3c0}: Run sleep and echo finished |  | omp/v18_field_working.txt | working status-spinner-elapsed
omp |  |  | omp/v18_blocked_tool_approval.txt | blocked tool-approval-dialog
omp |  |  | omp/v18_field_blocked_tool_approval.txt | blocked tool-approval-dialog
omp |  |  | omp/v18_blocked_ask.txt | nothing
omp | \u{3c0} > Run sleep and echo finished |  | @omp18-dialog-above-idle | not-blocked
omp |  |  | @omp18-dialog-above-idle | not-blocked
";
