use super::State;
use utoipa_axum::{router::OpenApiRouter, routes};

mod get {
    use crate::response::{ApiResponse, ApiResponseResult};
    use serde::Serialize;
    use std::path::Path;
    use sysinfo::{Disks, Networks, System};
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct CpuStats {
        used: f32,
        threads: usize,
        model: String,
    }

    #[derive(ToSchema, Serialize)]
    struct NetworkStats {
        received: u64,
        receiving_rate: f64,
        sent: u64,
        sending_rate: f64,
    }

    #[derive(ToSchema, Serialize)]
    struct MemoryStats {
        used: u64,
        total: u64,
    }

    #[derive(ToSchema, Serialize)]
    struct DiskStats {
        used: u64,
        total: u64,
        read: u64,
        reading_rate: f64,
        written: u64,
        writing_rate: f64,
    }

    #[derive(ToSchema, Serialize)]
    pub struct Response {
        #[schema(inline)]
        cpu: CpuStats,
        #[schema(inline)]
        network: NetworkStats,
        #[schema(inline)]
        memory: MemoryStats,
        #[schema(inline)]
        disk: DiskStats,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = OK, body = inline(Response)),
    ))]
    pub async fn route() -> ApiResponseResult {
        let mut sys = System::new_all();

        let mut disks = Disks::new_with_refreshed_list();
        let mut networks = Networks::new_with_refreshed_list();

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        sys.refresh_all();
        disks.refresh(true);
        networks.refresh(true);

        let total_memory = sys.total_memory() / 1024 / 1024;
        let used_memory = sys.used_memory() / 1024 / 1024;

        let disk = disks
            .iter()
            .find(|d| d.mount_point() == Path::new("/"))
            .unwrap_or(&disks[0]);
        let total_disk_space = disk.total_space() / 1024 / 1024;
        let used_disk_space = (disk.total_space() - disk.available_space()) / 1024 / 1024;
        let total_disk_read = disk.usage().total_read_bytes / 1024 / 1024;
        let disk_read_rate = disk.usage().read_bytes as f64 / 1024.0 / 1024.0;
        let total_disk_write = disk.usage().total_written_bytes / 1024 / 1024;
        let disk_write_rate = disk.usage().written_bytes as f64 / 1024.0 / 1024.0;

        let mut total_received = 0;
        let mut net_in_rate = 0.0;
        let mut total_transmitted = 0;
        let mut net_out_rate = 0.0;
        for (_, network) in networks.into_iter() {
            total_received += network.total_received() / 1024 / 1024;
            net_in_rate += network.received() as f64 / 1024.0 / 1024.0;
            total_transmitted += network.total_transmitted() / 1024 / 1024;
            net_out_rate += network.transmitted() as f64 / 1024.0 / 1024.0;
        }

        let cpu_usage = sys.global_cpu_usage();
        let cpu_threads = sys.cpus().len();
        let cpu_model = sys
            .cpus()
            .first()
            .map_or_else(|| "unknown".to_string(), |cpu| cpu.brand().to_string());

        ApiResponse::json(Response {
            cpu: CpuStats {
                used: cpu_usage,
                threads: cpu_threads,
                model: cpu_model,
            },
            network: NetworkStats {
                received: total_received,
                receiving_rate: net_in_rate,
                sent: total_transmitted,
                sending_rate: net_out_rate,
            },
            memory: MemoryStats {
                used: used_memory,
                total: total_memory,
            },
            disk: DiskStats {
                used: used_disk_space,
                total: total_disk_space,
                read: total_disk_read,
                reading_rate: disk_read_rate,
                written: total_disk_write,
                writing_rate: disk_write_rate,
            },
        })
        .ok()
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
