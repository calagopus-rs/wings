use super::State;
use std::{
    sync::{Arc, LazyLock, Mutex},
    time::Instant,
};
use utoipa_axum::{router::OpenApiRouter, routes};

struct MetricHistory {
    timestamps: Vec<Instant>,
    disk_reads: Vec<u64>,
    disk_writes: Vec<u64>,
    net_ins: Vec<u64>,
    net_outs: Vec<u64>,
    max_entries: usize,
}

impl MetricHistory {
    fn new(max_entries: usize) -> Self {
        Self {
            timestamps: Vec::with_capacity(max_entries),
            disk_reads: Vec::with_capacity(max_entries),
            disk_writes: Vec::with_capacity(max_entries),
            net_ins: Vec::with_capacity(max_entries),
            net_outs: Vec::with_capacity(max_entries),
            max_entries,
        }
    }

    fn add(
        &mut self,
        timestamp: Instant,
        disk_read: u64,
        disk_write: u64,
        net_in: u64,
        net_out: u64,
    ) {
        self.timestamps.push(timestamp);
        self.disk_reads.push(disk_read);
        self.disk_writes.push(disk_write);
        self.net_ins.push(net_in);
        self.net_outs.push(net_out);

        if self.timestamps.len() > self.max_entries {
            self.timestamps.remove(0);
            self.disk_reads.remove(0);
            self.disk_writes.remove(0);
            self.net_ins.remove(0);
            self.net_outs.remove(0);
        }
    }

    fn calculate_rates(&self) -> (f64, f64, f64, f64) {
        if self.timestamps.len() < 2 {
            return (0.0, 0.0, 0.0, 0.0);
        }

        let oldest_idx = 0;
        let newest_idx = self.timestamps.len() - 1;

        let elapsed_seconds = self.timestamps[newest_idx]
            .duration_since(self.timestamps[oldest_idx])
            .as_secs_f64();

        let elapsed_seconds = if elapsed_seconds <= 0.1 {
            0.1
        } else {
            elapsed_seconds
        };

        let disk_read_diff =
            self.disk_reads[newest_idx].saturating_sub(self.disk_reads[oldest_idx]) as f64;
        let disk_write_diff =
            self.disk_writes[newest_idx].saturating_sub(self.disk_writes[oldest_idx]) as f64;
        let net_in_diff = self.net_ins[newest_idx].saturating_sub(self.net_ins[oldest_idx]) as f64;
        let net_out_diff =
            self.net_outs[newest_idx].saturating_sub(self.net_outs[oldest_idx]) as f64;

        let disk_read_rate = disk_read_diff / elapsed_seconds;
        let disk_write_rate = disk_write_diff / elapsed_seconds;
        let net_in_rate = net_in_diff / elapsed_seconds;
        let net_out_rate = net_out_diff / elapsed_seconds;

        let disk_read_rate = if disk_read_diff > 0.0 && disk_read_rate < 0.01 {
            0.01
        } else {
            disk_read_rate
        };
        let disk_write_rate = if disk_write_diff > 0.0 && disk_write_rate < 0.01 {
            0.01
        } else {
            disk_write_rate
        };
        let net_in_rate = if net_in_diff > 0.0 && net_in_rate < 0.01 {
            0.01
        } else {
            net_in_rate
        };
        let net_out_rate = if net_out_diff > 0.0 && net_out_rate < 0.01 {
            0.01
        } else {
            net_out_rate
        };

        (disk_read_rate, disk_write_rate, net_in_rate, net_out_rate)
    }
}

static HISTORY: LazyLock<Arc<Mutex<MetricHistory>>> =
    LazyLock::new(|| Arc::new(Mutex::new(MetricHistory::new(5))));
static FIRST_RUN: LazyLock<Arc<Mutex<bool>>> = LazyLock::new(|| Arc::new(Mutex::new(true)));

mod get {
    use crate::routes::api::stats::{FIRST_RUN, HISTORY};
    use serde::Serialize;
    use std::{path::Path, time::Instant};
    use sysinfo::{Disks, Networks, System};
    use utoipa::ToSchema;

    #[derive(ToSchema, Serialize)]
    struct CpuStats {
        used: f64,
        threads: usize,
        model: String,
    }

    #[derive(ToSchema, Serialize)]
    struct NetworkStats {
        received: u64,
        recieving_rate: f64,
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
        cpu: CpuStats,
        network: NetworkStats,
        memory: MemoryStats,
        disk: DiskStats,
    }

    #[utoipa::path(get, path = "/", responses(
        (status = 200, body = inline(Response)),
    ))]
    pub async fn route() -> axum::Json<Response> {
        let mut sys = System::new_all();

        let mut disks = Disks::new_with_refreshed_list();
        let mut networks = Networks::new_with_refreshed_list();

        tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;

        sys.refresh_all();
        disks.refresh(true);
        networks.refresh(true);

        let total_memory = sys.total_memory() / (1024 * 1024);
        let used_memory = sys.used_memory() / (1024 * 1024);

        let disk = disks
            .iter()
            .find(|d| d.mount_point() == Path::new("/"))
            .unwrap_or(&disks[0]);
        let total_disk_space = disk.total_space() / (1024 * 1024);
        let used_disk_space = (disk.total_space() - disk.available_space()) / (1024 * 1024);
        let total_disk_read = disk.usage().total_read_bytes / (1024 * 1024);
        let total_disk_write = disk.usage().total_written_bytes / (1024 * 1024);

        let mut total_received = 0;
        let mut total_transmitted = 0;
        for (_, network) in networks.into_iter() {
            total_received += network.total_received() / (1024 * 1024);
            total_transmitted += network.total_transmitted() / (1024 * 1024);
        }

        let cpu_usage = sys.global_cpu_usage() as f64;
        let cpu_threads = sys.cpus().len();
        let cpu_model = sys
            .cpus()
            .first()
            .map_or_else(|| "unknown".to_string(), |cpu| cpu.brand().to_string());

        let now = Instant::now();
        let mut is_first_run = FIRST_RUN.lock().unwrap();
        let mut history = HISTORY.lock().unwrap();

        history.add(
            now,
            total_disk_read,
            total_disk_write,
            total_received,
            total_transmitted,
        );

        let (disk_read_rate, disk_write_rate, net_in_rate, net_out_rate) = if *is_first_run {
            *is_first_run = false;
            (0.0, 0.0, 0.0, 0.0)
        } else {
            history.calculate_rates()
        };

        axum::Json(Response {
            cpu: CpuStats {
                used: cpu_usage,
                threads: cpu_threads,
                model: cpu_model,
            },
            network: NetworkStats {
                received: total_received,
                recieving_rate: net_in_rate,
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
    }
}

pub fn router(state: &State) -> OpenApiRouter<State> {
    OpenApiRouter::new()
        .routes(routes!(get::route))
        .with_state(state.clone())
}
