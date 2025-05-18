use axum::{extract::ConnectInfo, http::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_default::DefaultFromSerde;
use std::{
    cell::UnsafeCell,
    collections::HashMap,
    ops::{Deref, DerefMut},
    os::unix::fs::PermissionsExt,
    sync::Arc,
};

pub static mut DEBUG: bool = false;

fn app_name() -> String {
    "Pterodactyl".to_string()
}
fn api_host() -> String {
    "0.0.0.0".to_string()
}
fn api_port() -> u16 {
    8080
}
fn api_upload_limit() -> usize {
    100
}

fn system_root_directory() -> String {
    "/var/lib/pterodactyl".to_string()
}
fn system_log_directory() -> String {
    "/var/log/pterodactyl".to_string()
}
fn system_data() -> String {
    "/var/lib/pterodactyl/volumes".to_string()
}
fn system_archive_directory() -> String {
    "/var/lib/pterodactyl/archives".to_string()
}
fn system_backup_directory() -> String {
    "/var/lib/pterodactyl/backups".to_string()
}
fn system_tmp_directory() -> String {
    "/tmp/pterodactyl".to_string()
}
fn system_username() -> String {
    "pterodactyl".to_string()
}
fn system_passwd_directory() -> String {
    "/run/wings/etc".to_string()
}
fn system_disk_check_interval() -> u64 {
    150
}
fn system_activity_send_interval() -> u64 {
    60
}
fn system_activity_send_count() -> usize {
    100
}
fn system_check_permissions_on_boot() -> bool {
    true
}
fn system_enable_log_rotate() -> bool {
    true
}
fn system_websocket_log_count() -> usize {
    150
}

fn system_sftp_address() -> String {
    "0.0.0.0".to_string()
}
fn system_sftp_port() -> u16 {
    2022
}

fn system_crash_detection_enabled() -> bool {
    true
}
fn system_crash_detection_detect_clean_exit_as_crash() -> bool {
    true
}
fn system_crash_detection_timeout() -> u64 {
    60
}

fn docker_network_interface() -> String {
    "172.18.0.1".to_string()
}
fn docker_network_dns() -> Vec<String> {
    vec!["1.1.1.1".to_string(), "1.0.0.1".to_string()]
}
fn docker_network_name() -> String {
    "pterodactyl_nw".to_string()
}
fn docker_network_driver() -> String {
    "bridge".to_string()
}
fn docker_network_mode() -> String {
    "pterodactyl_nw".to_string()
}
fn docker_network_enable_icc() -> bool {
    true
}
fn docker_network_network_mtu() -> u64 {
    1500
}

fn docker_network_interfaces_v4_subnet() -> String {
    "172.18.0.0/16".to_string()
}
fn docker_network_interfaces_v4_gateway() -> String {
    "172.18.0.1".to_string()
}
fn docker_network_interfaces_v6_subnet() -> String {
    "fdba:17c8:6c94::/64".to_string()
}
fn docker_network_interfaces_v6_gateway() -> String {
    "fdba:17c8:6c94::1011".to_string()
}

fn docker_tmpfs_size() -> u64 {
    100
}
fn docker_container_pid_limit() -> i64 {
    512
}

fn docker_installer_limits_memory() -> u64 {
    1024
}
fn docker_installer_limits_cpu() -> u64 {
    100
}

fn docker_overhead_default_multiplier() -> f64 {
    1.05
}

fn docker_log_config_config() -> HashMap<String, String> {
    HashMap::from([
        ("max-size".to_string(), "5m".to_string()),
        ("max-file".to_string(), "1".to_string()),
        ("compress".to_string(), "false".to_string()),
        ("mode".to_string(), "non-blocking".to_string()),
    ])
}

fn throttles_enabled() -> bool {
    true
}
fn throttles_lines() -> u64 {
    2000
}
fn throttles_line_reset_interval() -> u64 {
    100
}

fn remote_query_timeout() -> u64 {
    30
}
fn remote_query_boot_servers_per_page() -> u64 {
    50
}

nestify::nest! {
    #[derive(Deserialize, Serialize, DefaultFromSerde)]
    pub struct InnerConfig {
        #[serde(default)]
        pub debug: bool,
        #[serde(default = "app_name")]
        pub app_name: String,
        #[serde(default)]
        pub uuid: String,

        #[serde(default)]
        pub token_id: String,
        #[serde(default)]
        pub token: String,

        #[serde(default)]
        pub api: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct Api {
            #[serde(default = "api_host")]
            pub host: String,
            #[serde(default = "api_port")]
            pub port: u16,

            #[serde(default)]
            pub ssl: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct ApiSsl {
                #[serde(default)]
                pub enabled: bool,
                #[serde(default)]
                pub cert: String,
                #[serde(default)]
                pub key: String,
            },

            #[serde(default)]
            pub disable_remote_download: bool,
            #[serde(default = "api_upload_limit")]
            /// MB
            pub upload_limit: usize,
            #[serde(default)]
            pub trusted_proxies: Vec<std::net::IpAddr>,
        },
        #[serde(default)]
        pub system: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct System {
            #[serde(default = "system_root_directory")]
            pub root_directory: String,
            #[serde(default = "system_log_directory")]
            pub log_directory: String,
            #[serde(default = "system_data", rename = "data")]
            pub data_directory: String,
            #[serde(default = "system_archive_directory")]
            pub archive_directory: String,
            #[serde(default = "system_backup_directory")]
            pub backup_directory: String,
            #[serde(default = "system_tmp_directory")]
            pub tmp_directory: String,
            #[serde(default = "system_username")]
            pub username: String,

            #[serde(default)]
            pub user: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct SystemUser {
                #[serde(default)]
                pub rootless: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct SystemUserRootless {
                    #[serde(default)]
                    pub enabled: bool,
                    #[serde(default)]
                    pub container_uid: u32,
                    #[serde(default)]
                    pub container_gid: u32,
                },

                #[serde(default)]
                pub uid: u32,
                #[serde(default)]
                pub gid: u32,
            },

            #[serde(default)]
            pub passwd: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct SystemPasswd {
                #[serde(default)]
                pub enabled: bool,
                #[serde(default = "system_passwd_directory")]
                pub directory: String,
            },

            #[serde(default = "system_disk_check_interval")]
            pub disk_check_interval: u64,
            #[serde(default = "system_activity_send_interval")]
            pub activity_send_interval: u64,
            #[serde(default = "system_activity_send_count")]
            pub activity_send_count: usize,
            #[serde(default = "system_check_permissions_on_boot")]
            pub check_permissions_on_boot: bool,
            #[serde(default = "system_enable_log_rotate")]
            pub enable_log_rotate: bool,
            #[serde(default = "system_websocket_log_count")]
            pub websocket_log_count: usize,

            #[serde(default)]
            pub sftp: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct SystemSftp {
                #[serde(default = "system_sftp_address")]
                pub address: String,
                #[serde(default = "system_sftp_port")]
                pub port: u16,

                #[serde(default)]
                pub read_only: bool,
            },

            #[serde(default)]
            pub crash_detection: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct SystemCrashDetection {
                #[serde(default = "system_crash_detection_enabled")]
                pub enabled: bool,
                #[serde(default = "system_crash_detection_detect_clean_exit_as_crash")]
                pub detect_clean_exit_as_crash: bool,
                #[serde(default = "system_crash_detection_timeout")]
                pub timeout: u64,
            },

            #[serde(default)]
            pub backups: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct SystemBackups {
                #[serde(default)]
                /// MiB/s
                pub write_limit: u64,
                #[serde(default)]
                pub compression_level: #[derive(Clone, Copy, Deserialize, Serialize, Default)] #[serde(rename_all = "snake_case")] pub enum SystemBackupsCompressionLevel {
                    None,
                    #[default]
                    BestSpeed,
                    BestCompression,
                }
            },

            #[serde(default)]
            pub transfers: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct SystemTransfers {
                #[serde(default)]
                /// MiB/s
                pub download_limit: u64,
            },
        },
        #[serde(default)]
        pub docker: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct Docker {
            #[serde(default)]
            pub network: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct DockerNetwork {
                #[serde(default = "docker_network_interface")]
                pub interface: String,
                #[serde(default = "docker_network_dns")]
                pub dns: Vec<String>,

                #[serde(default = "docker_network_name")]
                pub name: String,
                #[serde(default)]
                pub ispn: bool,
                #[serde(default = "docker_network_driver")]
                pub driver: String,
                #[serde(default = "docker_network_mode")]
                pub mode: String,
                #[serde(default)]
                pub is_internal: bool,
                #[serde(default = "docker_network_enable_icc")]
                pub enable_icc: bool,
                #[serde(default = "docker_network_network_mtu")]
                pub network_mtu: u64,

                #[serde(default)]
                pub interfaces: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct DockerNetworkInterfaces {
                    #[serde(default)]
                    pub v4: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct DockerNetworkInterfacesV4 {
                        #[serde(default = "docker_network_interfaces_v4_subnet")]
                        pub subnet: String,
                        #[serde(default = "docker_network_interfaces_v4_gateway")]
                        pub gateway: String,
                    },
                    #[serde(default)]
                    pub v6: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct DockerNetworkInterfacesV6 {
                        #[serde(default = "docker_network_interfaces_v6_subnet")]
                        pub subnet: String,
                        #[serde(default = "docker_network_interfaces_v6_gateway")]
                        pub gateway: String,
                    },
                },
            },

            #[serde(default)]
            pub domainname: String,
            #[serde(default)]
            pub registries: HashMap<String, #[derive(Deserialize, Serialize)] pub struct DockerRegistryConfiguration {
                pub username: String,
                pub password: String,
            }>,

            #[serde(default = "docker_tmpfs_size")]
            pub tmpfs_size: u64,
            #[serde(default = "docker_container_pid_limit")]
            pub container_pid_limit: i64,

            #[serde(default)]
            pub installer_limits: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct DockerInstallerLimits {
                #[serde(default = "docker_installer_limits_memory")]
                /// MiB
                pub memory: u64,
                #[serde(default = "docker_installer_limits_cpu")]
                /// %
                pub cpu: u64,
            },

            #[serde(default)]
            pub overhead: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct DockerOverhead {
                #[serde(default)]
                pub r#override: bool,
                #[serde(default = "docker_overhead_default_multiplier")]
                pub default_multiplier: f64,

                #[serde(default)]
                /// Memory Limit MiB -> Multiplier
                pub multipliers: HashMap<i64, f64>,
            },

            #[serde(default)]
            pub userns_mode: String,

            #[serde(default)]
            pub log_config: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct DockerLogConfig {
                #[serde(default)]
                pub r#type: #[derive(Deserialize, Serialize, Default)] #[serde(rename_all = "snake_case")] pub enum DockerLogConfigType {
                    None,
                    #[default]
                    Local,
                },
                #[serde(default = "docker_log_config_config")]
                pub config: HashMap<String, String>,
            },
        },

        #[serde(default)]
        pub throttles: #[derive(Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct Throttles {
            #[serde(default = "throttles_enabled")]
            pub enabled: bool,
            #[serde(default = "throttles_lines")]
            pub lines: u64,
            #[serde(default = "throttles_line_reset_interval")]
            pub line_reset_interval: u64,
        },

        pub remote: String,
        #[serde(default)]
        pub remote_query: #[derive(Clone, Deserialize, Serialize, DefaultFromSerde)] #[serde(default)] pub struct RemoteQuery {
            #[serde(default = "remote_query_timeout")]
            pub timeout: u64,
            #[serde(default = "remote_query_boot_servers_per_page")]
            pub boot_servers_per_page: u64,
        },

        #[serde(default)]
        pub allowed_mounts: Vec<String>,
        #[serde(default)]
        pub allowed_origins: Vec<String>,

        #[serde(default)]
        pub allow_cors_private_network: bool,
        #[serde(default)]
        pub ignore_panel_config_updates: bool,
    }
}

impl DockerOverhead {
    /// ```yaml
    /// multipliers:
    ///   1024: 1.05
    ///   2048: 1.10
    /// ```
    /// means, <=1024MiB ram = 1.05 multiplier,
    /// <=2048MiB ram = 1.10 multiplier,
    /// >2048MiB ram = 1.05 multiplier (default_multiplier)
    pub fn get_mutiplier(&self, memory: i64) -> f64 {
        if !self.r#override {
            if memory <= 2048 {
                return 1.15;
            } else if memory <= 4096 {
                return 1.10;
            }

            return 1.05;
        }

        let mut multipliers = self.multipliers.keys().cloned().collect::<Vec<i64>>();
        multipliers.sort();
        multipliers.reverse();

        for m in multipliers {
            if memory > m {
                continue;
            }

            return self.multipliers[&m];
        }

        self.default_multiplier
    }

    pub fn get_memory(&self, memory: i64) -> i64 {
        let multiplier = self.get_mutiplier(memory);

        (memory as f64 * multiplier) as i64
    }
}

impl From<SystemBackupsCompressionLevel> for u32 {
    fn from(value: SystemBackupsCompressionLevel) -> Self {
        match value {
            SystemBackupsCompressionLevel::None => 0,
            SystemBackupsCompressionLevel::BestSpeed => 1,
            SystemBackupsCompressionLevel::BestCompression => 9,
        }
    }
}

pub struct Config {
    inner: UnsafeCell<InnerConfig>,

    pub path: String,
    pub client: crate::remote::client::Client,
    pub jwt: crate::remote::jwt::JwtClient,
}

unsafe impl Send for Config {}
unsafe impl Sync for Config {}

impl Config {
    pub fn open(path: &str) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);
        let config: InnerConfig = serde_yml::from_reader(reader)?;

        let client = crate::remote::client::Client::new(&config);
        let jwt = crate::remote::jwt::JwtClient::new(&config.token);
        let mut config = Self {
            inner: UnsafeCell::new(config),

            path: path.to_string(),
            client,
            jwt,
        };

        unsafe {
            DEBUG = config.debug;
        }

        config.ensure_directories()?;
        config.ensure_user()?;
        config.ensure_passwd()?;
        config.save()?;

        Ok(Arc::new(config))
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let file = std::fs::File::create(&self.path)?;
        let writer = std::io::BufWriter::new(file);
        serde_yml::to_writer(writer, unsafe { &*self.inner.get() })?;

        Ok(())
    }

    pub fn update(&self, new_config: InnerConfig) -> Result<(), Box<dyn std::error::Error>> {
        let config = unsafe { &mut *self.inner.get() };
        let _ = std::mem::replace(config, new_config);

        self.save()?;

        Ok(())
    }

    #[inline]
    pub fn find_ip(
        &self,
        headers: &HeaderMap,
        connect_info: ConnectInfo<std::net::SocketAddr>,
    ) -> std::net::IpAddr {
        for ip in &self.api.trusted_proxies {
            if connect_info.ip() == *ip {
                if let Some(forwarded) = headers.get("X-Forwarded-For") {
                    if let Ok(forwarded) = forwarded.to_str() {
                        if let Some(ip) = forwarded.split(',').next() {
                            return ip.parse().unwrap_or_else(|_| connect_info.ip());
                        }
                    }
                }

                if let Some(forwarded) = headers.get("X-Real-IP") {
                    if let Ok(forwarded) = forwarded.to_str() {
                        return forwarded.parse().unwrap_or_else(|_| connect_info.ip());
                    }
                }
            }
        }

        connect_info.ip()
    }

    #[allow(clippy::mut_from_ref)]
    fn unsafe_mut(&self) -> &mut InnerConfig {
        unsafe { &mut *self.inner.get() }
    }

    fn ensure_directories(&self) -> std::io::Result<()> {
        let directories = vec![
            &self.system.root_directory,
            &self.system.log_directory,
            &self.system.data_directory,
            &self.system.archive_directory,
            &self.system.backup_directory,
            &self.system.tmp_directory,
        ];

        for dir in directories {
            if !std::path::Path::new(dir).exists() {
                std::fs::create_dir_all(dir)?;
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }

        if self.system.passwd.enabled
            && !std::path::Path::new(&self.system.passwd.directory).exists()
        {
            std::fs::create_dir_all(&self.system.passwd.directory)?;
            std::fs::set_permissions(
                &self.system.passwd.directory,
                std::fs::Permissions::from_mode(0o755),
            )?;
        }

        Ok(())
    }

    fn ensure_user(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let release = std::fs::read_to_string("/etc/os-release")?;

        if release.contains("distroless") {
            self.system.username =
                std::env::var("WINGS_USERNAME").unwrap_or_else(|_| system_username());
            self.system.user.uid = std::env::var("WINGS_UID")
                .unwrap_or_else(|_| "988".to_string())
                .parse()?;
            self.system.user.gid = std::env::var("WINGS_GID")
                .unwrap_or_else(|_| "988".to_string())
                .parse()?;

            return Ok(());
        }

        if self.system.user.rootless.enabled {
            let user = users::get_current_uid();
            let group = users::get_current_gid();
            let username = users::get_current_username();

            self.system.username = username.unwrap().into_string().unwrap();
            self.system.user.uid = user;
            self.system.user.gid = group;

            return Ok(());
        }

        if let Some(user) = users::get_user_by_name(&self.system.username) {
            self.system.user.uid = user.uid();
            self.system.user.gid = user.primary_group_id();

            return Ok(());
        }

        let command = if release.contains("alpine") {
            std::process::Command::new("addgroup")
                .arg("-S")
                .arg(&self.system.username)
                .output()?;

            format!(
                "adduser -S -D -H -G {} -s /sbin/nologin {}",
                self.system.username, self.system.username
            )
        } else {
            format!(
                "useradd --system --no-create-home --shell /usr/sbin/nologin {}",
                self.system.username
            )
        };

        let split = command.split_whitespace().collect::<Vec<_>>();
        let output = std::process::Command::new(split[0])
            .args(&split[1..])
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "Failed to create user {}: {}",
                self.system.username,
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }

        let user = users::get_user_by_name(&self.system.username)
            .ok_or_else(|| format!("User {} not found", self.system.username))?;

        self.system.user.uid = user.uid();
        self.system.user.gid = user.primary_group_id();

        Ok(())
    }

    fn ensure_passwd(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.system.passwd.enabled {
            let v = format!(
                "root:x:0:\ncontainer:x:{}:\nnogroup:x:65534:",
                self.system.user.gid
            );
            std::fs::write(
                std::path::Path::new(&self.system.passwd.directory).join("group"),
                v,
            )?;
            std::fs::set_permissions(
                std::path::Path::new(&self.system.passwd.directory).join("group"),
                std::fs::Permissions::from_mode(0o644),
            )?;

            let v = format!(
                "root:x:0:0::/root:/bin/sh\ncontainer:x:{}:{}::/home/container:/bin/sh\nnobody:x:65534:65534::/var/empty:/bin/sh\n",
                self.system.user.uid, self.system.user.gid
            );
            std::fs::write(
                std::path::Path::new(&self.system.passwd.directory).join("passwd"),
                v,
            )?;
            std::fs::set_permissions(
                std::path::Path::new(&self.system.passwd.directory).join("passwd"),
                std::fs::Permissions::from_mode(0o644),
            )?;
        }

        Ok(())
    }

    pub async fn ensure_network(
        &self,
        client: &bollard::Docker,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let network = client
            .inspect_network::<String>(&self.docker.network.name, None)
            .await;

        if network.is_err() {
            client
                .create_network(bollard::network::CreateNetworkOptions {
                    name: self.docker.network.name.clone(),
                    driver: self.docker.network.driver.clone(),
                    enable_ipv6: true,
                    internal: self.docker.network.is_internal,
                    ipam: bollard::models::Ipam {
                        config: Some(vec![
                            bollard::models::IpamConfig {
                                subnet: Some(self.docker.network.interfaces.v4.subnet.clone()),
                                gateway: Some(self.docker.network.interfaces.v4.gateway.clone()),
                                ..Default::default()
                            },
                            bollard::models::IpamConfig {
                                subnet: Some(self.docker.network.interfaces.v6.subnet.clone()),
                                gateway: Some(self.docker.network.interfaces.v6.gateway.clone()),
                                ..Default::default()
                            },
                        ]),
                        ..Default::default()
                    },
                    options: HashMap::from([
                        ("encryption".to_string(), "false".to_string()),
                        (
                            "com.docker.network.bridge.default_bridge".to_string(),
                            "false".to_string(),
                        ),
                        (
                            "com.docker.network.bridge.enable_icc".to_string(),
                            self.docker.network.enable_icc.to_string(),
                        ),
                        (
                            "com.docker.network.bridge.enable_ip_masquerade".to_string(),
                            "true".to_string(),
                        ),
                        (
                            "com.docker.network.bridge.host_binding_ipv4".to_string(),
                            "0.0.0.0".to_string(),
                        ),
                        (
                            "com.docker.network.bridge.name".to_string(),
                            "pterodactyl0".to_string(),
                        ),
                        (
                            "com.docker.network.driver.mtu".to_string(),
                            self.docker.network.network_mtu.to_string(),
                        ),
                    ]),
                    ..Default::default()
                })
                .await?;

            let driver = &self.docker.network.driver;
            if driver != "host" && driver != "overlay" && driver != "weavemesh" {
                self.unsafe_mut().docker.network.interface =
                    self.docker.network.interfaces.v4.gateway.clone();
            }
        }

        match self.docker.network.driver.as_str() {
            "host" => {
                self.unsafe_mut().docker.network.interface = "127.0.0.1".to_string();
            }
            "overlay" | "weavemesh" => {
                self.unsafe_mut().docker.network.interface = "".to_string();
                self.unsafe_mut().docker.network.ispn = true;
            }
            _ => {
                self.unsafe_mut().docker.network.ispn = false;
            }
        }

        self.save()?;

        Ok(())
    }
}

impl Deref for Config {
    type Target = InnerConfig;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner.get() }
    }
}

impl DerefMut for Config {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.inner.get() }
    }
}
