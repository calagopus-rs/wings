use serde::{Deserialize, Serialize};

pub mod backups;
pub mod client;
pub mod jwt;
pub mod servers;

#[derive(Deserialize, Serialize)]
pub struct Pagination {
    current_page: usize,
    from: usize,
    last_page: usize,
    per_page: usize,
    to: usize,
    total: usize,
}
