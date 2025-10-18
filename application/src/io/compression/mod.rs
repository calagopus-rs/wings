use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub mod reader;
pub mod writer;

#[derive(Clone, Copy)]
pub enum CompressionType {
    None,
    Gz,
    Xz,
    Bz2,
    Lz4,
    Zstd,
}

#[derive(Clone, Copy, ToSchema, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
#[schema(rename_all = "snake_case")]
pub enum CompressionLevel {
    #[default]
    BestSpeed,
    GoodSpeed,
    GoodCompression,
    BestCompression,
}

impl CompressionLevel {
    #[inline]
    pub const fn to_deflate_level(self) -> u32 {
        match self {
            CompressionLevel::BestSpeed => 1,
            CompressionLevel::GoodSpeed => 4,
            CompressionLevel::GoodCompression => 6,
            CompressionLevel::BestCompression => 9,
        }
    }

    #[inline]
    pub const fn to_xz_level(self) -> u32 {
        match self {
            CompressionLevel::BestSpeed => 1,
            CompressionLevel::GoodSpeed => 4,
            CompressionLevel::GoodCompression => 6,
            CompressionLevel::BestCompression => 9,
        }
    }

    #[inline]
    pub const fn to_bz2_level(self) -> u32 {
        match self {
            CompressionLevel::BestSpeed => 1,
            CompressionLevel::GoodSpeed => 4,
            CompressionLevel::GoodCompression => 6,
            CompressionLevel::BestCompression => 9,
        }
    }

    #[inline]
    pub const fn to_zstd_level(self) -> i32 {
        match self {
            CompressionLevel::BestSpeed => 1,
            CompressionLevel::GoodSpeed => 8,
            CompressionLevel::GoodCompression => 14,
            CompressionLevel::BestCompression => 22,
        }
    }

    #[inline]
    pub const fn to_lzma2_level(self) -> u32 {
        match self {
            CompressionLevel::BestSpeed => 1,
            CompressionLevel::GoodSpeed => 4,
            CompressionLevel::GoodCompression => 6,
            CompressionLevel::BestCompression => 9,
        }
    }

    #[inline]
    pub const fn to_lz4_level(self) -> i32 {
        match self {
            CompressionLevel::BestSpeed => 1,
            CompressionLevel::GoodSpeed => 5,
            CompressionLevel::GoodCompression => 8,
            CompressionLevel::BestCompression => 12,
        }
    }
}
