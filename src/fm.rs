//! Démodulation FM large bande (WBFM) → audio mono → fichier WAV.
//!
//! Chaîne d'un vrai récepteur FM :
//!   1. filtre passe-bas + décimation de l'IQ vers ~200 kHz (isole LE canal FM,
//!      rejette stations voisines et bruit — étape cruciale pour la qualité) ;
//!   2. discriminateur de phase `angle(s·conj(s_prev))` = fréquence instantanée ;
//!   3. filtre passe-bas audio 15 kHz + décimation vers 50 kHz (vire pilote 19 kHz,
//!      sous-porteuse stéréo, RDS) ;
//!   4. désaccentuation 50 µs, retrait du continu, normalisation.

use std::f32::consts::PI;
use std::fs::File;
use std::io::{self, Write};

const IQ_RATE: u32 = 2_000_000;
/// Étage 1 : 2,0 MS/s → 250 kHz (canal FM, marge anti-repliement à Nyquist 125 kHz).
const DECIM1: usize = 8;
const IF_RATE: u32 = IQ_RATE / DECIM1 as u32; // 250 kHz
/// Étage 2 : 250 kHz → 50 kHz (audio).
const DECIM2: usize = 5;
pub const AUDIO_RATE: u32 = IF_RATE / DECIM2 as u32; // 50 kHz

/// Amplitude RMS d'un buffer IQ (pour repérer les stations).
pub fn rms(iq: &[u8]) -> f64 {
    let pairs = iq.len() / 2;
    if pairs == 0 {
        return 0.0;
    }
    let mut sumsq = 0f64;
    for c in iq.chunks_exact(2) {
        let i = f64::from(c[0]) - 127.5;
        let q = f64::from(c[1]) - 127.5;
        sumsq += i * i + q * q;
    }
    (sumsq / pairs as f64).sqrt()
}

/// Coefficients d'un passe-bas sinc fenêtré (Hamming), gain unité en continu.
/// `fc` = fréquence de coupure normalisée (cycles/échantillon, 0..0,5).
pub(crate) fn lowpass_taps(n: usize, fc: f32) -> Vec<f32> {
    let mut taps = vec![0f32; n];
    let m = (n - 1) as f32 / 2.0;
    let mut sum = 0f32;
    for (i, t) in taps.iter_mut().enumerate() {
        let x = i as f32 - m;
        let sinc = if x.abs() < 1e-6 {
            2.0 * fc
        } else {
            (2.0 * PI * fc * x).sin() / (PI * x)
        };
        let w = 0.54 - 0.46 * (2.0 * PI * i as f32 / (n - 1) as f32).cos(); // Hamming
        *t = sinc * w;
        sum += *t;
    }
    for t in &mut taps {
        *t /= sum;
    }
    taps
}

/// Filtre passe-bas complexe (sur l'IQ u8) + décimation. Renvoie (I, Q) en f32.
fn fir_decimate_complex(iq: &[u8], taps: &[f32], decim: usize) -> (Vec<f32>, Vec<f32>) {
    let n = iq.len() / 2;
    let nt = taps.len();
    let mut oi = Vec::new();
    let mut oq = Vec::new();
    if n < nt {
        return (oi, oq);
    }
    let mut pos = 0;
    while pos + nt <= n {
        let mut si = 0f32;
        let mut sq = 0f32;
        for (k, &t) in taps.iter().enumerate() {
            let idx = pos + k;
            si += t * (f32::from(iq[2 * idx]) - 127.5);
            sq += t * (f32::from(iq[2 * idx + 1]) - 127.5);
        }
        oi.push(si);
        oq.push(sq);
        pos += decim;
    }
    (oi, oq)
}

/// Filtre passe-bas réel + décimation.
fn fir_decimate_real(x: &[f32], taps: &[f32], decim: usize) -> Vec<f32> {
    let nt = taps.len();
    let mut out = Vec::new();
    if x.len() < nt {
        return out;
    }
    let mut pos = 0;
    while pos + nt <= x.len() {
        let mut s = 0f32;
        for (k, &t) in taps.iter().enumerate() {
            s += t * x[pos + k];
        }
        out.push(s);
        pos += decim;
    }
    out
}

/// Étage 1 : isole le canal FM (passe-bas ~110 kHz) et décime vers IF_RATE.
fn channelize(iq: &[u8]) -> (Vec<f32>, Vec<f32>) {
    let chan_taps = lowpass_taps(127, 110_000.0 / IQ_RATE as f32);
    fir_decimate_complex(iq, &chan_taps, DECIM1)
}

/// Étage 2 : discriminateur de phase → fréquence instantanée (rad/échantillon).
fn discriminate(i: &[f32], q: &[f32]) -> Vec<f32> {
    let mut demod = Vec::with_capacity(i.len());
    if i.is_empty() {
        return demod;
    }
    let (mut pi, mut pq) = (i[0], q[0]);
    demod.push(0.0);
    for k in 1..i.len() {
        let re = i[k] * pi + q[k] * pq;
        let im = q[k] * pi - i[k] * pq;
        demod.push(im.atan2(re));
        pi = i[k];
        pq = q[k];
    }
    demod
}

/// Démod FM bande étroite pour NOAA APT (satellite au centre d'accord).
pub fn demod_apt(iq: &[u8]) -> (Vec<f32>, f64) {
    demod_apt_shifted(iq, 0.0)
}

/// Démod APT d'un satellite situé à `offset_hz` du centre d'accord : un NCO
/// numérique ramène sa porteuse en bande de base avant le filtre de canal.
/// Permet de décoder **plusieurs satellites depuis une seule capture large bande**.
/// Canal ±22 kHz, décimation ×40 → 50 kHz, discriminateur (sans désaccentuation).
pub fn demod_apt_shifted(iq: &[u8], offset_hz: f64) -> (Vec<f32>, f64) {
    const DECIM: usize = 40; // 2,0 MS/s → 50 kHz
    let taps = lowpass_taps(127, 22_000.0 / IQ_RATE as f32);
    let nt = taps.len();
    let n = iq.len() / 2;

    // NCO par récurrence (rotation d'un phaseur), renormalisé périodiquement.
    let dphi = 2.0 * std::f64::consts::PI * offset_hz / f64::from(IQ_RATE);
    let (dc, ds) = (dphi.cos(), dphi.sin());
    let (mut pc, mut ps) = (1.0f64, 0.0f64);

    // Fenêtre glissante (ring) des nt derniers échantillons mixés ; FIR + décimation
    // calculés seulement aux positions de sortie (efficace en mémoire et en CPU).
    let mut wi = vec![0f32; nt];
    let mut wq = vec![0f32; nt];
    let mut head = 0usize; // position du plus ancien
    let mut filled = 0usize;
    let mut since = 0usize;
    let mut di = Vec::with_capacity(n / DECIM + 1);
    let mut dq = Vec::with_capacity(n / DECIM + 1);

    for k in 0..n {
        let i = f32::from(iq[2 * k]) - 127.5;
        let q = f32::from(iq[2 * k + 1]) - 127.5;
        let (c, s) = (pc as f32, ps as f32);
        // (i+jq) · e^{-jθ}  ramène la composante à +offset vers 0
        wi[head] = i * c + q * s;
        wq[head] = q * c - i * s;
        head = (head + 1) % nt;
        if filled < nt {
            filled += 1;
        }
        // rotation du phaseur de +dphi
        let npc = pc * dc - ps * ds;
        let nps = pc * ds + ps * dc;
        pc = npc;
        ps = nps;
        if k & 0xff_ffff == 0 {
            let m = (pc * pc + ps * ps).sqrt();
            pc /= m;
            ps /= m;
        }
        since += 1;
        if filled == nt && since >= DECIM {
            since = 0;
            let mut si = 0f32;
            let mut sq = 0f32;
            for (t, &tap) in taps.iter().enumerate() {
                let idx = (head + t) % nt; // head = plus ancien
                si += tap * wi[idx];
                sq += tap * wq[idx];
            }
            di.push(si);
            dq.push(sq);
        }
    }
    let demod = discriminate(&di, &dq);
    (demod, f64::from(IQ_RATE) / DECIM as f64)
}

/// Estime le décalage de la porteuse (Hz) : c'est la fréquence moyenne du
/// discriminateur (l'audio étant de moyenne nulle).
pub fn estimate_offset_hz(iq: &[u8]) -> i32 {
    let (i, q) = channelize(iq);
    let demod = discriminate(&i, &q);
    if demod.len() < 16 {
        return 0;
    }
    let mean = demod.iter().sum::<f32>() / demod.len() as f32;
    (mean * IF_RATE as f32 / (2.0 * PI)) as i32
}

/// Démodule un buffer IQ WBFM en audio mono 16 bits à [`AUDIO_RATE`].
/// `hp_hz` = fréquence du passe-haut anti-rumble (≈ 60 Hz par défaut).
pub fn demod_wbfm(iq: &[u8], hp_hz: f32) -> Vec<i16> {
    if iq.len() / 2 < 4 {
        return Vec::new();
    }

    let (i, q) = channelize(iq);
    let demod = discriminate(&i, &q);

    // Passe-bas audio 15 kHz + décimation ×5 → 50 kHz mono.
    let audio_taps = lowpass_taps(127, 15_000.0 / IF_RATE as f32);
    let mut audio = fir_decimate_real(&demod, &audio_taps, DECIM2);

    // 4) Désaccentuation 50 µs (filtre 1 pôle).
    let dt = 1.0 / AUDIO_RATE as f32;
    let tau = 50e-6;
    let a = dt / (tau + dt);
    let mut y = 0f32;
    for s in &mut audio {
        y += a * (*s - y);
        *s = y;
    }

    // 5) Passe-haut 2 pôles à `hp_hz` : retire rumble/ronflement sous-grave
    //    (12 dB/oct, bien plus efficace sur un 50 Hz qu'un simple 1 pôle).
    let r = (-2.0 * PI * hp_hz / AUDIO_RATE as f32).exp();
    for _ in 0..2 {
        let (mut x1, mut y1) = (0f32, 0f32);
        for s in &mut audio {
            let y = *s - x1 + r * y1;
            x1 = *s;
            y1 = y;
            *s = y;
        }
    }

    // 6) Normalisation par un quasi-pic (99,9e percentile) avec marge, puis
    //    limiteur DOUX (tanh) : les rares pics sont arrondis en douceur au lieu
    //    d'être tranchés net (pas de distorsion dure sur les graves).
    let mut mags: Vec<f32> = audio.iter().map(|v| v.abs()).collect();
    mags.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((mags.len() as f32 * 0.999) as usize).min(mags.len() - 1);
    let peak = mags[idx].max(1e-9);
    let gain = 0.9 * 32767.0 / peak;
    let limit = 32767.0;

    // --- Diagnostic objectif : énergie cumulée dans 3 sous-bandes graves ---
    let rms = (audio.iter().map(|v| v * v).sum::<f32>() / audio.len().max(1) as f32).sqrt();
    let a = |fc: f32| 1.0 - (-2.0 * PI * fc / AUDIO_RATE as f32).exp();
    let (a40, a90, a150) = (a(40.0), a(90.0), a(150.0));
    let (mut l40, mut l90, mut l150) = (0f32, 0f32, 0f32);
    let (mut e40, mut e90, mut e150, mut et) = (0f64, 0f64, 0f64, 0f64);
    for &v in &audio {
        l40 += a40 * (v - l40);
        l90 += a90 * (v - l90);
        l150 += a150 * (v - l150);
        e40 += f64::from(l40 * l40);
        e90 += f64::from(l90 * l90);
        e150 += f64::from(l150 * l150);
        et += f64::from(v * v);
    }
    let et = et.max(1e-9);
    let limited = audio.iter().filter(|&&v| (v * gain).abs() > 0.99 * limit).count() as f64
        / audio.len().max(1) as f64;
    eprintln!(
        "   [diag] énergie <40Hz={:.0}%  <90Hz={:.0}%  <150Hz={:.0}%  crête/RMS={:.1}  limiteur={:.2}%",
        e40 / et * 100.0,
        e90 / et * 100.0,
        e150 / et * 100.0,
        rms.max(1e-9).recip() * audio.iter().fold(0f32, |m, &v| m.max(v.abs())),
        limited * 100.0,
    );

    audio
        .iter()
        .map(|&v| (limit * (v * gain / limit).tanh()) as i16)
        .collect()
}

/// Écrit un WAV PCM 16 bits mono.
pub fn write_wav(path: &str, samples: &[i16], rate: u32) -> io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut f = File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for &s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}
