use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{Local, TimeZone};
use shared::{GpsStatus, Pass, SatImage};
use std::path::PathBuf;

const PASSAGES_DIR: &str = "/home/hugues/sdr/passages";
const TLE_CACHE_DIR: &str = "/home/hugues/sdr/tle_cache";
const DIST_DIR: &str = "/home/hugues/sdr/web/frontend/dist";

#[tokio::main]
async fn main() {
    let cors = tower_http::cors::CorsLayer::permissive();
    let app = Router::new()
        .route("/api/gps", get(api_gps))
        .route("/api/passes", get(api_passes))
        .route("/api/images", get(api_images))
        .route("/images/*path", get(serve_image))
        .fallback_service(
            tower_http::services::ServeDir::new(DIST_DIR)
                .append_index_html_on_directories(true),
        )
        .layer(cors);

    println!("🌐 SDR Station Web — http://0.0.0.0:8080");
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ── GPS ───────────────────────────────────────────────────────────────────────

async fn api_gps() -> Json<GpsStatus> {
    Json(get_gps())
}

fn get_gps() -> GpsStatus {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let Ok(mut child) = Command::new("gpspipe")
        .args(["-r", "-n", "40"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return GpsStatus::default();
    };

    let reader = BufReader::new(match child.stdout.take() {
        Some(o) => o,
        None => return GpsStatus::default(),
    });

    let mut result = GpsStatus::default();
    for line in reader.lines().flatten().take(40) {
        if let Some((lat, lon)) = parse_nmea(&line) {
            result = GpsStatus { fix: true, lat, lon };
            break;
        }
    }
    let _ = child.kill();
    result
}

fn parse_nmea(line: &str) -> Option<(f64, f64)> {
    let f: Vec<&str> = line.split(',').collect();
    let kind = f.first()?;
    if kind.ends_with("GGA") && f.len() >= 7 && f[6] != "0" {
        Some((nmea_deg(f[2], f[3])?, nmea_deg(f[4], f[5])?))
    } else if kind.ends_with("RMC") && f.len() >= 7 && f[2] == "A" {
        Some((nmea_deg(f[3], f[4])?, nmea_deg(f[5], f[6])?))
    } else {
        None
    }
}

fn nmea_deg(val: &str, hemi: &str) -> Option<f64> {
    let dot = val.find('.')?;
    if dot < 2 {
        return None;
    }
    let (deg_str, min_str) = val.split_at(dot - 2);
    let deg: f64 = deg_str.parse().ok()?;
    let min: f64 = min_str.parse().ok()?;
    let mut d = deg + min / 60.0;
    if hemi == "S" || hemi == "W" {
        d = -d;
    }
    Some(d)
}

// ── Passes ────────────────────────────────────────────────────────────────────

async fn api_passes() -> Json<Vec<Pass>> {
    Json(compute_passes())
}

fn compute_passes() -> Vec<Pass> {
    use std::f64::consts::PI;

    const SATS: &[(&str, u32, f64)] = &[
        ("NOAA 15", 25338, 137.620),
        ("NOAA 18", 28654, 137.9125),
        ("NOAA 19", 33591, 137.100),
        ("Meteor-M2-3", 57166, 137.900),
        ("Meteor-M2-4", 59051, 137.100),
    ];

    let obs = (48.7956_f64 * PI / 180.0, 2.3144_f64 * PI / 180.0, 0.07_f64);

    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(_) => return vec![],
    };

    let mut passes: Vec<Pass> = Vec::new();

    for &(name, catnr, freq) in SATS {
        let tle_path = format!("{TLE_CACHE_DIR}/{catnr}.tle");
        let Ok(tle) = std::fs::read_to_string(&tle_path) else { continue };
        let lines: Vec<&str> = tle.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
        let Some(l1) = lines.iter().find(|l| l.starts_with("1 ")).copied() else { continue };
        let Some(l2) = lines.iter().find(|l| l.starts_with("2 ")).copied() else { continue };
        let Ok(elements) = sgp4::Elements::from_tle(None, l1.as_bytes(), l2.as_bytes()) else { continue };
        let epoch_ts = elements.datetime.and_utc().timestamp() as f64;
        let Ok(constants) = sgp4::Constants::from_elements(&elements) else { continue };

        let (mut in_pass, mut aos, mut max_el) = (false, 0.0_f64, -90.0_f64);
        let mut t = now;

        while t <= now + 48.0 * 3600.0 {
            let mins = (t - epoch_ts) / 60.0;
            let el = constants
                .propagate(sgp4::MinutesSinceEpoch(mins))
                .map(|p| elevation(p.position, obs, t))
                .unwrap_or(-90.0);

            if el > 0.0 {
                if !in_pass {
                    aos = t;
                    in_pass = true;
                }
                if el > max_el {
                    max_el = el;
                }
            } else if in_pass {
                let los = t;
                if max_el >= 5.0 {
                    let dur = (los - aos) / 60.0;
                    let Some(aos_dt) = Local.timestamp_opt(aos as i64, 0).single() else {
                        in_pass = false;
                        max_el = -90.0;
                        t += 30.0;
                        continue;
                    };
                    passes.push(Pass {
                        name: name.to_string(),
                        freq_mhz: freq,
                        aos_ts: aos as i64,
                        los_ts: los as i64,
                        aos_fmt: aos_dt.format("%a %d/%m %H:%M").to_string(),
                        duration_min: dur,
                        max_el,
                    });
                }
                in_pass = false;
                max_el = -90.0;
            }
            t += 30.0;
        }
    }

    passes.sort_by(|a, b| a.aos_ts.cmp(&b.aos_ts));
    passes
}

fn elevation(pos: [f64; 3], (obs_lat, obs_lon, obs_alt): (f64, f64, f64), unix: f64) -> f64 {
    use std::f64::consts::PI;
    let [sx, sy, sz] = pos;
    let d = unix / 86400.0 + 2_440_587.5 - 2_451_545.0;
    let th = (280.460_618_37 + 360.985_647_366_29 * d).rem_euclid(360.0) * PI / 180.0;
    let (st, ct) = th.sin_cos();
    let (ex, ey, ez) = (ct * sx + st * sy, -st * sx + ct * sy, sz);
    let a = 6378.137_f64;
    let e2 = 0.006_694_38_f64;
    let (sp, cp) = obs_lat.sin_cos();
    let (sl, cl) = obs_lon.sin_cos();
    let n = a / (1.0 - e2 * sp * sp).sqrt();
    let (ox, oy, oz) = (
        (n + obs_alt) * cp * cl,
        (n + obs_alt) * cp * sl,
        (n * (1.0 - e2) + obs_alt) * sp,
    );
    let (rx, ry, rz) = (ex - ox, ey - oy, ez - oz);
    let rng = (rx * rx + ry * ry + rz * rz).sqrt();
    let up = cp * cl * rx + cp * sl * ry + sp * rz;
    (up / rng).asin() * 180.0 / PI
}

// ── Images ────────────────────────────────────────────────────────────────────

async fn api_images() -> Json<Vec<SatImage>> {
    let mut images = Vec::new();
    collect_images(std::path::Path::new(PASSAGES_DIR), &mut images);
    images.sort_by(|a, b| b.captured_fmt.cmp(&a.captured_fmt));
    Json(images)
}

fn collect_images(dir: &std::path::Path, out: &mut Vec<SatImage>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_images(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("bmp" | "BMP" | "png" | "PNG")
        ) {
            if let Some(img) = parse_image_meta(&path) {
                out.push(img);
            }
        }
    }
}

fn parse_image_meta(path: &std::path::Path) -> Option<SatImage> {
    let stem = path.file_stem()?.to_str()?;
    let parts: Vec<&str> = stem.split('_').collect();
    let rel = path.strip_prefix(PASSAGES_DIR).ok()?;
    let url = format!("/images/{}", rel.to_string_lossy().replace('\\', "/"));

    let elevation = parts.iter()
        .find(|p| p.starts_with("el"))
        .and_then(|p| p[2..].parse::<f64>().ok())
        .unwrap_or(0.0);

    let satellite = parts.first().copied().unwrap_or("Unknown").replace('-', " ");

    let captured_fmt = if parts.len() >= 3 {
        let date = parts[1];
        let time = parts[2];
        if date.len() == 8 && time.len() == 4 {
            format!("{}/{}/{} {}:{}", &date[6..8], &date[4..6], &date[..4], &time[..2], &time[2..])
        } else {
            format!("{} {}", date, time)
        }
    } else {
        "Inconnu".to_string()
    };

    Some(SatImage { filename: stem.to_string(), url, satellite, captured_fmt, elevation })
}

async fn serve_image(Path(path): Path<String>) -> Response {
    if path.contains("..") {
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }
    let full = PathBuf::from(PASSAGES_DIR).join(&path);
    if !full.exists() {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    if path.to_lowercase().ends_with(".png") {
        let data = std::fs::read(&full).unwrap_or_default();
        return ([(header::CONTENT_TYPE, "image/png")], data).into_response();
    }
    match image::open(&full) {
        Ok(img) => {
            let mut buf = Vec::new();
            let _ = img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png);
            ([(header::CONTENT_TYPE, "image/png")], buf).into_response()
        }
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Convert error").into_response(),
    }
}
