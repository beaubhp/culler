use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
        )]
        pub struct $name(u32);

        impl $name {
            pub const fn new(raw: u32) -> Self {
                Self(raw)
            }

            pub const fn as_u32(self) -> u32 {
                self.0
            }
        }
    };
}

id_type!(FileId);
id_type!(ModuleId);
id_type!(DefId);
id_type!(ScopeId);
id_type!(ContextId);
id_type!(SymbolId);
id_type!(BindingId);
id_type!(BindingSetId);
id_type!(FlowUncertaintySetId);
id_type!(ReferenceId);
id_type!(LoopId);
