/// One tool call in flight, borrowing its arguments. `Send`, because the
/// operation ledger may finish a call in the background.
pub(crate) type ToolFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, ToolError>> + Send + 'a>>;

/// How a tool runs once its name is matched.
#[derive(Clone, Copy)]
pub(crate) enum Handler {
    /// In-process, from its arguments alone.
    InProcess(for<'a> fn(&'a Value) -> ToolFuture<'a>),
    /// In-process, by a handler that several tools share and that reads the
    /// tool's own name.
    ByName(for<'a> fn(&'static str, &'a Value) -> ToolFuture<'a>),
    /// The canonical CLI, through the adapter `crate::tool_table` admits for
    /// this tool's residue row.
    Cli(for<'a> fn(&'a Value, &'a crate::cli_adapter::CliAdapter) -> ToolFuture<'a>),
}

/// One MCP tool: its name, its `tools/list` entry, and its handler.
pub(crate) struct ToolSpec {
    pub(crate) name: &'static str,
    pub(crate) schema: fn() -> Value,
    pub(crate) handler: Handler,
}

impl ToolSpec {
    /// A tool that runs in-process from its arguments.
    pub(crate) const fn in_process(
        name: &'static str,
        schema: fn() -> Value,
        call: for<'a> fn(&'a Value) -> ToolFuture<'a>,
    ) -> Self {
        Self { name, schema, handler: Handler::InProcess(call) }
    }

    /// A tool whose shared in-process handler reads the tool's name.
    pub(crate) const fn by_name(
        name: &'static str,
        schema: fn() -> Value,
        call: for<'a> fn(&'static str, &'a Value) -> ToolFuture<'a>,
    ) -> Self {
        Self { name, schema, handler: Handler::ByName(call) }
    }

    /// A tool that runs the canonical CLI through its residue adapter.
    pub(crate) const fn cli(
        name: &'static str,
        schema: fn() -> Value,
        call: for<'a> fn(&'a Value, &'a crate::cli_adapter::CliAdapter) -> ToolFuture<'a>,
    ) -> Self {
        Self { name, schema, handler: Handler::Cli(call) }
    }
}

/// Every family's tools, in catalog order.
const REGISTRY: &[&[ToolSpec]] = &[
    CORE_SPECS,
    crate::ask_tool::SPECS,
    crate::cli_tools::SPECS,
    crate::plugin_tools::SPECS,
    crate::org_tools::SPECS,
    crate::agent_tools::SPECS,
    crate::diagnostic_tools::SPECS,
    crate::perf_tools::SPECS,
    crate::coord_tools::SPECS,
    crate::resource_tools::SPECS,
    crate::document_tools::SPECS,
    crate::document_tools::apps::SPECS,
    crate::approval_tools::SPECS,
    crate::terminal_tools::SPECS,
];

/// Every registered tool, in catalog order.
pub(crate) fn registry() -> impl Iterator<Item = &'static ToolSpec> {
    REGISTRY.iter().flat_map(|family| family.iter())
}

/// The registered tool called `name`.
fn lookup(name: &str) -> Option<&'static ToolSpec> {
    registry().find(|spec| spec.name == name)
}

/// Dispatch a `tools/call` by tool name.
///
/// # Errors
///
/// An unknown tool, a malformed argument, or any underlying failure.
pub(crate) async fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    // ADR-0128: confirm before any parsing, socket resolution, or traffic.
    crate::annotations::require_confirmation(name, args)?;
    let spec = lookup(name).ok_or_else(|| ToolError::new(format!("unknown tool: {name}")))?;
    run(spec, args).await
}

/// Run `spec`'s handler on `args`.
async fn run(spec: &'static ToolSpec, args: &Value) -> Result<Value, ToolError> {
    match spec.handler {
        Handler::InProcess(call) => call(args).await,
        Handler::ByName(call) => call(spec.name, args).await,
        Handler::Cli(call) => {
            let adapter = crate::cli_adapter::CliAdapter::for_residue(spec.name)?;
            call(args, &adapter).await
        }
    }
}

/// Run the registered tool `name` without the confirmation gate `dispatch`
/// puts first, so a test reaches the handler's own argument checks.
#[cfg(test)]
pub(crate) async fn call_registered(name: &str, args: &Value) -> Result<Value, ToolError> {
    let spec = lookup(name).ok_or_else(|| ToolError::new(format!("unknown tool: {name}")))?;
    run(spec, args).await
}

/// Run the CLI-backed tool `name` through `adapter`, so a test can stand a
/// fake `phux` in for the canonical CLI.
#[cfg(test)]
pub(crate) async fn call_with_adapter(
    name: &str,
    args: &Value,
    adapter: &crate::cli_adapter::CliAdapter,
) -> Result<Value, ToolError> {
    match lookup(name).map(|spec| spec.handler) {
        Some(Handler::Cli(call)) => call(args, adapter).await,
        _ => Err(ToolError::new(format!("not a CLI-backed tool: {name}"))),
    }
}
