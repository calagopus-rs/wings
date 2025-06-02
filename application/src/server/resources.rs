use super::state::ServerState;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

nestify::nest! {
    #[derive(ToSchema, Default, Deserialize, Serialize, Clone, Copy, PartialEq)]
    pub struct ResourceUsage {
        pub memory_bytes: u64,
        pub memory_limit_bytes: u64,
        pub disk_bytes: u64,

        pub state: ServerState,

        #[schema(inline)]
        pub network: #[derive(ToSchema, Default, Deserialize, Serialize, Clone, Copy, PartialEq)] pub struct ResourceUsageNetwork {
            pub rx_bytes: u64,
            pub tx_bytes: u64,
        },

        pub cpu_absolute: f64,
        pub uptime: u64,
    }
}
