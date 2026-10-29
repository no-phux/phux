/// Dispatch a `tools/call` by tool name.
///
/// # Errors
///
/// An unknown tool, a malformed argument, or any underlying failure.
pub(crate) async fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    // ADR-0128: confirm before any parsing, socket resolution, or traffic.
    crate::annotations::require_confirmation(name, args)?;
    match name {
        "phux_terminal_create" => crate::terminal_tools::create(args).await,
        "phux_terminal_operation" => crate::terminal_tools::operation(args).await,
        "phux_api_describe" => crate::terminal_tools::describe(args).await,
        "phux_ls" => phux_ls(args).await,
        "phux_snapshot" => phux_snapshot(args).await,
        "phux_send_keys" => phux_send_keys(args).await,
        "phux_paste" => phux_paste(args).await,
        "phux_upload" => phux_upload(args).await,
        "phux_run" => phux_run(args).await,
        "phux_wait" => phux_wait(args).await,
        "phux_new" => phux_new(args).await,
        "phux_kill" => phux_kill(args).await,
        "phux_detach" => phux_detach(args).await,
        "phux_watch" => phux_watch(args).await,
        "phux_ask" => crate::ask_tool::call(args).await,
        "phux_launch"
        | "phux_spawn"
        | "phux_signal"
        | "phux_tag"
        | "phux_rename"
        | "phux_insert_pane"
        | "phux_move_pane"
        | "phux_swap_pane"
        | "phux_archive"
        | "phux_project_inspect" => crate::cli_tools::call(name, args).await,
        "phux_plugin_action" => crate::plugin_tools::action(args).await,
        "phux_plugin_profile" => crate::plugin_tools::profile(args),
        family => dispatch_family(family, args).await,
    }
}

/// The tools each family module owns by name.
async fn dispatch_family(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        org if crate::org_tools::owns(org) => crate::org_tools::call(org, args).await,
        agent if crate::agent_tools::owns(agent) => crate::agent_tools::call(agent, args).await,
        diagnostic if crate::diagnostic_tools::owns(diagnostic) => {
            crate::diagnostic_tools::call(diagnostic, args).await
        }
        perf if crate::perf_tools::owns(perf) => crate::perf_tools::call(perf, args).await,
        coord if crate::coord_tools::owns(coord) => crate::coord_tools::call(coord, args).await,
        resource if crate::resource_tools::owns(resource) => {
            crate::resource_tools::call(resource, args).await
        }
        approval if crate::approval_tools::owns(approval) => {
            crate::approval_tools::call(approval, args).await
        }
        other => Err(ToolError::new(format!("unknown tool: {other}"))),
    }
}
