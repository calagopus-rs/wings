use super::state::ServerState;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

nestify::nest! {
    #[derive(ToSchema, Deserialize, Serialize, Clone, Copy, PartialEq)]
    pub struct ResourceUsage {
        pub memory_bytes: u64,
        pub memory_limit_bytes: u64,
        pub disk_bytes: u64,

        pub state: ServerState,

        #[schema(inline)]
        pub network: #[derive(ToSchema, Deserialize, Serialize, Clone, Copy, PartialEq)] pub struct ResourceUsageNetwork {
            pub rx_bytes: u64,
            pub tx_bytes: u64,
        },

        pub cpu_absolute: f64,
        pub uptime: u64,
    }
}

impl Default for ResourceUsage {
    fn default() -> Self {
        Self {
            memory_bytes: 0,
            memory_limit_bytes: 0,
            disk_bytes: 0,
            state: ServerState::Offline,
            network: ResourceUsageNetwork {
                rx_bytes: 0,
                tx_bytes: 0,
            },
            cpu_absolute: 0.0,
            uptime: 0,
        }
    }
}
