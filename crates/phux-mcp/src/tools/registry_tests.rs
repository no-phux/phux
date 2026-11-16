#[cfg(test)]
mod registry_tests {
    use super::*;

    /// The registry and the parity table name the same tools: a tool is
    /// never served without a parity row, nor a row left without a tool.
    /// `phux_operation_status` is the adapter's own, outside the registry.
    #[test]
    fn the_registry_names_exactly_the_tool_table() {
        let mut registered: Vec<&str> = registry().map(|spec| spec.name).collect();
        registered.push("phux_operation_status");
        registered.sort_unstable();
        let unique = registered.len();
        registered.dedup();
        assert_eq!(registered.len(), unique, "a tool is registered twice");

        let mut table: Vec<&str> = crate::tool_table::TOOLS.iter().map(|row| row.name).collect();
        table.sort_unstable();
        assert_eq!(registered, table);
    }

    /// Each row's schema names the tool the row dispatches.
    #[test]
    fn every_spec_schema_carries_its_own_name() {
        for spec in registry() {
            assert_eq!((spec.schema)()["name"], spec.name);
        }
    }
}
