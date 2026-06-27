//! Prédiction des passages satellites par SGP4 + géométrie topocentrique.
//!
//! Télécharge les TLE à jour depuis Celestrak (via `curl`, intégré à Windows),
//! propage les orbites des NOAA par SGP4, et calcule l'élévation au-dessus de
//! l'observateur (Bagneux) pour en déduire les passages.

use std::f64::consts::PI;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

use std::sync::OnceLock;

/// Position de l'observateur (radians + km). Par défaut : Bagneux (92220).
pub struct Observer {
    pub lat: f64,
    pub lon: f64,
    pub alt_km: f64,
}

static OBSERVER: OnceLock<Observer> = OnceLock::new();

/// Observateur courant (Bagneux si rien n'a été défini via `set_observer`).
fn obs() -> &'static Observer {
    OBSERVER.get_or_init(|| Observer {
        lat: 48.7956 * PI / 180.0,
        lon: 2.3144 * PI / 180.0,
        alt_km: 0.07,
    })
}

/// Fixe la position de l'observateur (lat/lon en degrés). À appeler avant toute prédiction.
pub fn set_observer(lat_deg: f64, lon_deg: f64, alt_km: f64) {
    let _ = OBSERVER.set(Observer {
        lat: lat_deg * PI / 180.0,
        lon: lon_deg * PI / 180.0,
        alt_km,
    });
}

/// Périphérique GPS par défaut (surchargé par la variable d'env `SDR_GPS`).
fn gps_device() -> String {
    std::env::var("SDR_GPS").unwrap_or_else(|_| "/dev/ttyACM0".to_string())
}

/// Tente de localiser l'observateur via le GPS (récepteur u-blox sur `/dev/ttyACM0`).
/// Fixe la position si un fix est obtenu ; sinon laisse la position par défaut (Bagneux).
/// À appeler une fois avant la prédiction.
pub fn locate_via_gps() {
    let dev = gps_device();
    eprintln!("📡 lecture du GPS sur {dev}…");
    match gps_fix(&dev, 90) {
        Ok((lat, lon)) => {
            println!("📍 GPS : lat {lat:.4}, lon {lon:.4}");
            set_observer(lat, lon, 0.1);
        }
        Err(e) => eprintln!("⚠️  GPS indisponible ({e}) — position par défaut (Bagneux)"),
    }
}

/// Lit le flux NMEA et renvoie (lat, lon) en degrés au premier fix valide.
/// Essaie gpsd via `gpspipe -r` en priorité (évite le conflit de port),
/// puis accès direct au port série en repli si gpsd est absent.
pub fn gps_fix(device: &str, timeout_s: u64) -> Result<(f64, f64)> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    // Essai via gpsd (gpspipe -r).
    if let Ok(mut child) = Command::new("gpspipe")
        .args(["-r"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        let deadline = now_unix() + timeout_s as f64;
        let found = child.stdout.take().and_then(|out| {
            let reader = BufReader::new(out);
            for line in reader.lines() {
                let Ok(line) = line else { continue };
                if let Some(pos) = parse_nmea_fix(&line) {
                    return Some(pos);
                }
                if now_unix() > deadline {
                    break;
                }
            }
            None
        });
        let _ = child.kill();
        let _ = child.wait();
        if let Some(pos) = found {
            return Ok(pos);
        }
        return Err("pas de fix gpsd dans le délai (vue dégagée ? dehors ?)".into());
    }

    // Repli : accès direct au port série (gpsd non disponible).
    let file = std::fs::File::open(device).map_err(|e| format!("ouverture {device} : {e}"))?;
    let reader = BufReader::new(file);
    let deadline = now_unix() + timeout_s as f64;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if let Some(pos) = parse_nmea_fix(&line) {
            return Ok(pos);
        }
        if now_unix() > deadline {
            return Err("pas de fix dans le délai (vue dégagée ? dehors ?)".into());
        }
    }
    Err("flux GPS terminé sans fix".into())
}

/// Extrait (lat, lon) d'une phrase NMEA GGA ou RMC (n'importe quel talker GP/GN/GL).
/// Renvoie None si la phrase n'est pas une position valide.
fn parse_nmea_fix(line: &str) -> Option<(f64, f64)> {
    let f: Vec<&str> = line.split(',').collect();
    let kind = *f.first()?;
    if kind.ends_with("GGA") {
        // $..GGA,heure,lat,N/S,lon,E/W,qualité,...   (qualité 0 = pas de fix)
        if f.len() < 7 || f[6] == "0" {
            return None;
        }
        Some((nmea_deg(f[2], f[3])?, nmea_deg(f[4], f[5])?))
    } else if kind.ends_with("RMC") {
        // $..RMC,heure,statut,lat,N/S,lon,E/W,...    (statut A = actif)
        if f.len() < 7 || f[2] != "A" {
            return None;
        }
        Some((nmea_deg(f[3], f[4])?, nmea_deg(f[5], f[6])?))
    } else {
        None
    }
}

/// Convertit une coordonnée NMEA (ddmm.mmmm + hémisphère) en degrés décimaux signés.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nmea_gga_parse() {
        let s = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47";
        let (lat, lon) = parse_nmea_fix(s).expect("GGA doit donner un fix");
        assert!((lat - 48.1173).abs() < 1e-3, "lat={lat}");
        assert!((lon - 11.5167).abs() < 1e-3, "lon={lon}");
    }

    #[test]
    fn nmea_rmc_parse_south_west() {
        let s = "$GNRMC,081836,A,3751.65,S,14507.36,W,000.0,360.0,130998,011.3,E*62";
        let (lat, lon) = parse_nmea_fix(s).expect("RMC actif doit donner un fix");
        assert!(lat < 0.0 && lon < 0.0, "hémisphères S/W → négatifs : {lat},{lon}");
    }

    #[test]
    fn nmea_no_fix() {
        let s = "$GPGGA,123519,,,,,0,00,,,M,,M,,*47"; // qualité 0
        assert!(parse_nmea_fix(s).is_none());
    }
}

/// Satellites météo suivis : (nom, NORAD ID, fréquence MHz, décodable).
/// Les NOAA émettent en APT analogique (décodé par le module `apt`). Les Meteor-M
/// émettent en LRPT numérique (OQPSK) — désormais décodés via SatDump (module `satdump`).
const SATS: &[(&str, u32, f64, bool)] = &[
    ("NOAA 15", 25338, 137.6200, true),
    ("NOAA 18", 28654, 137.9125, true),
    ("NOAA 19", 33591, 137.1000, true),
    ("Meteor-M2-3", 57166, 137.9000, true),
    ("Meteor-M2-4", 59051, 137.1000, true),
];

pub struct Sat {
    pub name: String,
    pub freq_mhz: f64,
    pub decodable: bool,
    epoch_ts: f64,
    constants: sgp4::Constants,
}

pub struct Pass {
    pub name: String,
    pub freq_mhz: f64,
    pub decodable: bool,
    pub aos: f64,    // acquisition (unix s)
    pub los: f64,    // perte de signal (unix s)
    pub max_el: f64, // élévation max (degrés)
    #[allow(dead_code)]
    pub max_t: f64, // instant de l'élévation max (unix s)
}

pub fn now_unix() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

/// Télécharge les TLE et construit les propagateurs SGP4.
/// Tolérant : un satellite dont le TLE échoue est ignoré (avec avertissement).
pub fn fetch_sats() -> Result<Vec<Sat>> {
    let mut sats = Vec::new();
    for &(name, catnr, freq, decodable) in SATS {
        match fetch_one(name, catnr, freq, decodable) {
            Ok(s) => sats.push(s),
            Err(e) => eprintln!("   ⚠️  {name} ignoré : {e}"),
        }
    }
    if sats.is_empty() {
        return Err("aucun TLE récupéré (réseau ? curl absent ?)".into());
    }
    Ok(sats)
}

fn fetch_one(name: &str, catnr: u32, freq: f64, decodable: bool) -> Result<Sat> {
    // 1. Tentative en ligne (Celestrak). 2. Repli sur le cache local si hors-ligne.
    let url = format!("https://celestrak.org/NORAD/elements/gp.php?CATNR={catnr}&FORMAT=TLE");
    let online = Command::new("curl")
        .args(["-sL", "--max-time", "20", &url])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .filter(|t| has_tle(t));

    let text = match online {
        Some(t) => {
            save_tle_cache(catnr, &t); // rafraîchit le cache
            t
        }
        None => {
            eprintln!("   ℹ️  {name} : TLE depuis le cache local (hors-ligne)");
            read_tle_cache(catnr).ok_or("TLE indisponible (hors-ligne et aucun cache local)")?
        }
    };

    let lines: Vec<&str> = text.lines().map(|l| l.trim_end()).filter(|l| !l.is_empty()).collect();
    let l1 = lines.iter().find(|l| l.starts_with("1 ")).ok_or("TLE ligne 1 absente")?;
    let l2 = lines.iter().find(|l| l.starts_with("2 ")).ok_or("TLE ligne 2 absente")?;
    let elements = sgp4::Elements::from_tle(None, l1.as_bytes(), l2.as_bytes())?;
    let epoch_ts = elements.datetime.and_utc().timestamp() as f64;
    let constants = sgp4::Constants::from_elements(&elements)?;
    Ok(Sat { name: name.to_string(), freq_mhz: freq, decodable, epoch_ts, constants })
}

/// Vrai si le texte contient bien les deux lignes d'un TLE.
fn has_tle(text: &str) -> bool {
    let l1 = text.lines().any(|l| l.starts_with("1 "));
    let l2 = text.lines().any(|l| l.starts_with("2 "));
    l1 && l2
}

fn tle_cache_path(catnr: u32) -> std::path::PathBuf {
    std::path::PathBuf::from("tle_cache").join(format!("{catnr}.tle"))
}

fn save_tle_cache(catnr: u32, text: &str) {
    let p = tle_cache_path(catnr);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, text);
}

fn read_tle_cache(catnr: u32) -> Option<String> {
    std::fs::read_to_string(tle_cache_path(catnr)).ok().filter(|t| has_tle(t))
}

/// Temps sidéral de Greenwich (rad) pour un instant unix.
fn gmst(unix: f64) -> f64 {
    let jd = unix / 86400.0 + 2_440_587.5;
    let d = jd - 2_451_545.0;
    let t = d / 36525.0;
    let g = 280.460_618_37 + 360.985_647_366_29 * d + 0.000_387_933 * t * t - t * t * t / 38_710_000.0;
    g.rem_euclid(360.0) * PI / 180.0
}

/// Élévation (degrés) du satellite vue de l'observateur à l'instant `unix`.
pub fn elevation(sat: &Sat, unix: f64) -> f64 {
    let mins = (unix - sat.epoch_ts) / 60.0;
    let pred = match sat.constants.propagate(sgp4::MinutesSinceEpoch(mins)) {
        Ok(p) => p,
        Err(_) => return -90.0,
    };
    let [sx, sy, sz] = pred.position; // TEME (km)

    // TEME → ECEF (rotation de GMST autour de Z).
    let th = gmst(unix);
    let (sth, cth) = th.sin_cos();
    let ex = cth * sx + sth * sy;
    let ey = -sth * sx + cth * sy;
    let ez = sz;

    // Observateur en ECEF (WGS84).
    let o = obs();
    let a = 6378.137;
    let f = 1.0 / 298.257_223_563;
    let e2 = f * (2.0 - f);
    let (sphi, cphi) = o.lat.sin_cos();
    let (slon, clon) = o.lon.sin_cos();
    let n = a / (1.0 - e2 * sphi * sphi).sqrt();
    let ox = (n + o.alt_km) * cphi * clon;
    let oy = (n + o.alt_km) * cphi * slon;
    let oz = (n * (1.0 - e2) + o.alt_km) * sphi;

    // Vecteur observateur→satellite ; composante zénithale → élévation.
    let (rx, ry, rz) = (ex - ox, ey - oy, ez - oz);
    let rng = (rx * rx + ry * ry + rz * rz).sqrt();
    let up = cphi * clon * rx + cphi * slon * ry + sphi * rz;
    (up / rng).asin() * 180.0 / PI
}

/// Détecte tous les passages (élévation > 0) entre `t0` et `t1`.
pub fn find_passes(sats: &[Sat], t0: f64, t1: f64) -> Vec<Pass> {
    const STEP: f64 = 30.0;
    let mut passes = Vec::new();
    for sat in sats {
        let mut in_pass = false;
        let (mut aos, mut max_el, mut max_t) = (0.0, -90.0, 0.0);
        let mut t = t0;
        while t <= t1 {
            let el = elevation(sat, t);
            if el > 0.0 {
                if !in_pass {
                    in_pass = true;
                    aos = t;
                    max_el = el;
                    max_t = t;
                } else if el > max_el {
                    max_el = el;
                    max_t = t;
                }
            } else if in_pass {
                passes.push(Pass {
                    name: sat.name.clone(),
                    freq_mhz: sat.freq_mhz,
                    decodable: sat.decodable,
                    aos,
                    los: t,
                    max_el,
                    max_t,
                });
                in_pass = false;
            }
            t += STEP;
        }
    }
    passes.sort_by(|a, b| a.aos.partial_cmp(&b.aos).unwrap());
    passes
}

/// Affiche un tableau des passages (heure locale).
pub fn print_passes(passes: &[Pass]) {
    use chrono::{Local, TimeZone};
    println!(
        "   {:<9} {:<18} {:>5}  {:>7}  {:>9}",
        "Sat", "Début (local)", "Élév", "Durée", "Fréq MHz"
    );
    for p in passes {
        let dt = Local.timestamp_opt(p.aos as i64, 0).single().unwrap();
        let dur = (p.los - p.aos) / 60.0;
        let mark = if p.name.starts_with("Meteor") {
            if p.max_el >= 20.0 { " ★ bon (LRPT/SatDump)" } else { " (LRPT/SatDump)" }
        } else if p.max_el >= 20.0 {
            " ★ bon"
        } else {
            ""
        };
        println!(
            "   {:<12} {:<18} {:>4.0}°  {:>5.1}min  {:>9.4}{}",
            p.name,
            dt.format("%a %d/%m %H:%M"),
            p.max_el,
            dur,
            p.freq_mhz,
            mark
        );
    }
}
