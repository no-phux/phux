//! Public operation names, independent of wire carriers.

macro_rules! operations {
    ($($(#[$doc:meta])* $variant:ident => $name:literal),+ $(,)?) => {
        /// Operations with a public request and response contract.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum PublicOperation {
            $($(#[$doc])* $variant),+
        }

        impl PublicOperation {
            /// Name reported by connected discovery and accepted by public clients.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => $name),+ }
            }

            /// Find a typed public operation, never a raw wire method.
            #[must_use]
            pub fn parse(name: &str) -> Option<Self> {
                match name { $($name => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

operations! {
    /// Commit a new terminal's placement and launch intent.
    TerminalCreate => "terminal.create",
    /// Recover a retained creation receipt.
    TerminalOperation => "terminal.operation",
    /// Inspect a resource by exact durable key.
    ResourceShow => "resource.show",
    /// Read or wait for a single execution without following relaunches.
    ExecutionResult => "execution.result",
    /// Read a resource's current provider callbacks or advisory PTY evidence.
    QuestionList => "question.list",
    /// Submit a human response to an exact live provider callback.
    QuestionRespond => "question.respond",
    /// Read a retained response receipt without submitting a response.
    QuestionReceipt => "question.receipt",
}
