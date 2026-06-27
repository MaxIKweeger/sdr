use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct GpsStatus {
    pub fix: bool,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Pass {
    pub name: String,
    pub freq_mhz: f64,
    pub aos_ts: i64,
    pub los_ts: i64,
    pub aos_fmt: String,
    pub duration_min: f64,
    pub max_el: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SatImage {
    pub filename: String,
    pub url: String,
    pub satellite: String,
    pub captured_fmt: String,
    pub elevation: f64,
}
