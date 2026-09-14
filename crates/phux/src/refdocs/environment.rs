//! The generated environment-variable reference, rendered from the
//! canonical table in `crate::environment` — the same table the
//! `phux help environment` topic renders from, so the page and the topic
//! cannot disagree.

use crate::environment::ENV_VARS;

use super::Page;

/// Render `docs/reference/environment.md`.
pub(crate) fn page() -> Page {
    use std::fmt::Write as _;

    let mut body = String::from(
        "Every environment variable the `phux` binary reads, from the \
         canonical in-code table that also renders `phux help environment`. \
         A flag always wins over its variable (`--socket` over `PHUX_SOCKET`, \
         `phux server --quic` over `PHUX_QUIC_ADDR`).\n\n\
         | Variable | Meaning |\n\
         |---|---|\n",
    );
    for spec in ENV_VARS {
        let meaning = spec.lines.join(" ").replace('|', "\\|");
        let _ = writeln!(body, "| `{}` | {meaning} |", spec.name);
    }
    body.push_str(
        "\nRun `phux server --listen 127.0.0.1:8787` to expose a port; see \
         `phux server --help` for the remote and TLS details.\n",
    );

    Page {
        file: "environment.md",
        title: "phux environment variables reference",
        summary: "Every environment variable the binary reads and what each one does.",
        tldr: "The canonical environment-variable table: the socket path, the \
               remote listeners and their TLS and token material, the helper \
               programs (`ssh`, `tailscale`), the auto-spawn idle limit, and \
               logging. Rendered from the same in-code table `phux help \
               environment` uses, so the two cannot disagree.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::{ENV_VARS, page};

    /// One table row per variable, and no extras.
    #[test]
    fn environment_page_has_a_row_per_variable() {
        let page = page();
        for spec in ENV_VARS {
            assert!(
                page.body.contains(&format!("| `{}` |", spec.name)),
                "generated environment reference has no row for {}",
                spec.name
            );
        }
        let rows = page
            .body
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count();
        assert_eq!(
            rows,
            ENV_VARS.len(),
            "row count must match the canonical table exactly"
        );
    }
}
