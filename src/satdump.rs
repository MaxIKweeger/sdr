//! Décodage via SatDump lancé en sous-processus.
//!
//! Notre décodeur `apt` gère les NOAA (APT analogique). SatDump apporte en plus le
//! **Meteor-M LRPT numérique** (OQPSK), que l'APT Rust ne sait pas faire.
//!
//! Principe : l'IQ capturé est déjà au format RTL natif (octets u8 entrelacés I/Q).
//! On l'écrit tel quel dans un fichier baseband, puis on appelle
//! `satdump <pipeline> baseband <fichier> <sortie> --samplerate … --baseband_format … --freq_shift …`.
//!
//! Prérequis : SatDump installé et accessible (sur le PATH, ou via la variable
//! d'environnement `SATDUMP_BIN` qui pointe vers `satdump.exe`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Format baseband passé à SatDump.
/// SatDump 1.x (dont 1.2.2) attend `u8` ; une future 2.x utilisera `cu8`.
const BASEBAND_FORMAT: &str = "u8";

/// Exécutable SatDump (surchargé par la variable d'environnement `SATDUMP_BIN`).
fn bin() -> String {
    std::env::var("SATDUMP_BIN").unwrap_or_else(|_| "satdump".to_string())
}

/// Pipeline SatDump correspondant à un satellite, d'après son nom.
/// Renvoie `None` pour les satellites qu'on préfère décoder autrement (NOAA → APT Rust).
pub fn pipeline_for(name: &str) -> Option<&'static str> {
    if name.starts_with("Meteor") {
        // 72k. Variante 80k : "meteor_m2-x_lrpt_80k".
        Some("meteor_m2-x_lrpt")
    } else if name.starts_with("NOAA") {
        Some("noaa_apt")
    } else {
        None
    }
}

/// Vrai si SatDump est installé et lançable.
pub fn available() -> bool {
    Command::new(bin())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map(|mut child| {
            let _ = child.wait();
        })
        .is_ok()
}

/// Écrit le baseband u8 dans `out_dir/baseband.cu8` puis le décode avec SatDump.
/// Renvoie le chemin du baseband (conservé pour un éventuel re-décodage).
///
/// `freq_shift_hz` = fréquence satellite − fréquence centrale de capture
/// (0 si la capture est déjà centrée sur le satellite). Si l'image sort vide,
/// inverser le signe (la convention de `--freq_shift` peut varier selon la version).
pub fn decode(
    pipeline: &str,
    iq_u8: &[u8],
    samplerate: u32,
    freq_shift_hz: f64,
    out_dir: &Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let baseband = out_dir.join("baseband.cu8");
    std::fs::write(&baseband, iq_u8)?;
    decode_file(pipeline, &baseband, samplerate, freq_shift_hz, out_dir)?;
    Ok(baseband)
}

/// Décode un fichier baseband déjà présent sur le disque (réutilisable pour
/// plusieurs satellites avec des `freq_shift` différents).
pub fn decode_file(
    pipeline: &str,
    baseband: &Path,
    samplerate: u32,
    freq_shift_hz: f64,
    out_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let status = Command::new(bin())
        .arg(pipeline)
        .arg("baseband")
        .arg(baseband)
        .arg(out_dir)
        .args(["--samplerate", &samplerate.to_string()])
        .args(["--baseband_format", BASEBAND_FORMAT])
        .args(["--freq_shift", &format!("{freq_shift_hz:.0}")])
        .status()
        .map_err(|e| format!("impossible de lancer SatDump ('{}') : {e}", bin()))?;
    if !status.success() {
        return Err(format!("SatDump a échoué (code {:?})", status.code()).into());
    }
    Ok(())
}
