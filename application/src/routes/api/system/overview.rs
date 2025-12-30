use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::{
        response::{ApiResponse, ApiResponseResult},
        routes::GetState,
    };
    use serde::Serialize;
    use sysinfo::System;
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct ResponseCpu<'a> {
        name: &'a str,
        brand: &'a str,
        vendor_id: &'a str,
        frequency_mhz: u64,
        cpu_count: usize,
    }

    #[derive(ToSchema, Serialize)]
    struct ResponseMemory {
        total_bytes: u64,
        free_bytes: u64,
        used_bytes: u64,
    }

    #[derive(ToSchema, Serialize)]
    struct ResponseServers {
        total: usize,
        online: usize,
        offline: usize,
    }

    #[derive(ToSchema, Serialize)]
    struct Response<'a> {
        version: &'a str,
        container_type: crate::routes::AppContainerType,

        #[schema(inline)]
        cpu: ResponseCpu<'a>,
        #[schema(inline)]
        memory: ResponseMemory,
        #[schema(inline)]
        servers: ResponseServers,

        architecture: &'static str,
        kernel_version: String,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(Response)),
    ))]
    pub async fn route(state: GetState) -> ApiResponseResult {
        let mut sys = System::new_all();
        sys.refresh_cpu_all();

        let cpu = &sys.cpus()[0];
        let mut servers = ResponseServers {
            total: 0,
            online: 0,
            offline: 0,
        };

        for server in state.server_manager.get_servers().await.iter() {
            servers.total += 1;
            if server.state.get_state() == crate::server::state::ServerState::Offline {
                servers.offline += 1;
            } else {
                servers.online += 1;
            }
        }

        ApiResponse::json(Response {
            version: &state.version,
            container_type: state.container_type,
            cpu: ResponseCpu {
                name: cpu.name(),
                brand: cpu.brand(),
                vendor_id: cpu.vendor_id(),
                frequency_mhz: cpu.frequency(),
                cpu_count: sys.cpus().len(),
            },
            memory: ResponseMemory {
                total_bytes: sys.total_memory(),
                free_bytes: sys.free_memory(),
                used_bytes: sys.used_memory(),
            },
            servers,
            architecture: std::env::consts::ARCH,
            kernel_version: sysinfo::System::kernel_long_version(),
        })
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
