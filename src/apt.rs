//! Décodeur d'images météo NOAA APT (Automatic Picture Transmission), from scratch.
//!
//! Après démodulation FM du signal 137 MHz, on obtient un audio = sous-porteuse
//! 2400 Hz **modulée en amplitude** par la luminance de l'image. Format APT :
//!   - 2 lignes/seconde, 4160 pixels/seconde → 2080 pixels/ligne ;
//!   - chaque ligne : synchro A + image A (909 px) + télémétrie, puis synchro B
//!     + image B (909 px) + télémétrie (deux bandes spectrales côte à côte).
//!
//! Chaîne : resample → démod AM (enveloppe) → flux pixels → détection synchro →
//! assemblage des lignes → normalisation → BMP.

use std::f32::consts::PI;
use std::fs::{self, File};
use std::io::{self, Write};

/// Cadence de travail interne (5 échantillons par pixel).
const WORK_RATE: f64 = 20_800.0;
const PIXEL_RATE: f64 = 4_160.0;
const SAMPLES_PER_PIXEL: usize = (WORK_RATE / PIXEL_RATE) as usize; // 5
const LINE_PIXELS: usize = 2_080;
const SUBCARRIER: f32 = 2_400.0;

/// Coefficients d'un passe-bas sinc fenêtré (Hamming), gain unité en continu.
fn lowpass_taps(n: usize, fc: f32) -> Vec<f32> {
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
        let w = 0.54 - 0.46 * (2.0 * PI * i as f32 / (n - 1) as f32).cos();
        *t = sinc * w;
        sum += *t;
    }
    for t in &mut taps {
        *t /= sum;
    }
    taps
}

/// Convolution FIR « valide » (sortie = len - ntaps + 1).
fn fir_valid(x: &[f32], taps: &[f32]) -> Vec<f32> {
    let nt = taps.len();
    if x.len() < nt {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(x.len() - nt + 1);
    for i in 0..=(x.len() - nt) {
        let mut s = 0f32;
        for (k, &t) in taps.iter().enumerate() {
            s += t * x[i + k];
        }
        out.push(s);
    }
    out
}

/// Rééchantillonnage linéaire de `from` Hz vers `to` Hz.
fn resample_linear(x: &[f32], from: f64, to: f64) -> Vec<f32> {
    if x.is_empty() {
        return Vec::new();
    }
    let ratio = from / to;
    let out_n = (x.len() as f64 / ratio) as usize;
    let mut out = Vec::with_capacity(out_n);
    for i in 0..out_n {
        let pos = i as f64 * ratio;
        let i0 = pos.floor() as usize;
        let frac = (pos - i0 as f64) as f32;
        let a = x[i0];
        let b = if i0 + 1 < x.len() { x[i0 + 1] } else { a };
        out.push(a + (b - a) * frac);
    }
    out
}

/// Démodulation AM par élévation au carré + passe-bas : env = sqrt(2·LP(s²)).
fn am_envelope(sig: &[f32]) -> Vec<f32> {
    let sq: Vec<f32> = sig.iter().map(|s| s * s).collect();
    let taps = lowpass_taps(51, 2_000.0 / WORK_RATE as f32);
    let lp = fir_valid(&sq, &taps);
    lp.iter().map(|&v| (2.0 * v.max(0.0)).sqrt()).collect()
}

/// Motif de synchro A (±1) : 7 impulsions de 1040 Hz = 2 px haut / 2 px bas.
fn sync_a_ref() -> Vec<f32> {
    let mut s = vec![-1f32; 4]; // espace de tête
    for _ in 0..7 {
        s.extend_from_slice(&[1.0, 1.0, -1.0, -1.0]);
    }
    s.extend_from_slice(&[-1.0; 8]); // espace de queue
    s
}

/// Détecte les débuts de ligne (synchro A) dans le flux pixels, avec suivi de dérive.
fn find_line_starts(px: &[f32]) -> Vec<usize> {
    let syn = sync_a_ref();
    let sl = syn.len();
    let n = px.len();
    if n < LINE_PIXELS + sl {
        return Vec::new();
    }

    // Corrélation (moyenne retirée) du motif de synchro à chaque position.
    let mut corr = vec![f32::MIN; n];
    for i in 0..=(n - sl) {
        let mean: f32 = px[i..i + sl].iter().sum::<f32>() / sl as f32;
        let mut s = 0f32;
        for (k, &sv) in syn.iter().enumerate() {
            s += sv * (px[i + k] - mean);
        }
        corr[i] = s;
    }

    // Premier départ : meilleure corrélation dans la 1re ligne.
    let mut best = 0usize;
    let mut bestv = f32::MIN;
    for (i, &c) in corr.iter().enumerate().take(LINE_PIXELS.min(n)) {
        if c > bestv {
            bestv = c;
            best = i;
        }
    }

    // Suivi type PLL : période de ligne (~2080) + phase corrigées en douceur,
    // et seulement si la synchro est franche ; sinon on continue en roue libre.
    // Une zone bruitée donne alors du bruit propre, sans dérailler la suite.
    let thresh = bestv * 0.5; // synchro jugée « franche » au-dessus de ce seuil
    let win = 20i64;
    let mut starts = Vec::new();
    let mut pos = best as f64;
    let mut period = LINE_PIXELS as f64;

    while (pos as usize) + LINE_PIXELS <= n {
        starts.push(pos.round() as usize);
        let predicted = pos + period;

        // Cherche le pic de corrélation autour de la position prédite.
        let lo = ((predicted as i64) - win).max(0) as usize;
        let hi = (((predicted as i64) + win) as usize).min(corr.len() - 1);
        let (mut pk_pos, mut pk_val) = (predicted, f32::MIN);
        for c in lo..=hi {
            if corr[c] > pk_val {
                pk_val = corr[c];
                pk_pos = c as f64;
            }
        }

        if pk_val > thresh {
            // synchro franche : corrige phase (gain 0,3) et période (gain 0,02)
            let err = pk_pos - predicted;
            pos = predicted + 0.3 * err;
            period = (period + 0.02 * err).clamp(2070.0, 2090.0);
        } else {
            // synchro faible : roue libre à la période courante
            pos = predicted;
        }
    }
    starts
}

/// Décode un audio APT (échantillons mono, `rate` Hz) en image (octets, largeur, hauteur).
pub fn decode(samples: &[f32], rate: f64) -> (Vec<u8>, usize, usize) {
    // 1) Resample vers la cadence de travail.
    let work = resample_linear(samples, rate, WORK_RATE);
    // 2) Démod AM → enveloppe.
    let env = am_envelope(&work);
    // 3) Sous-échantillonnage à la cadence pixel (5 → 1).
    let px: Vec<f32> = env.iter().step_by(SAMPLES_PER_PIXEL).copied().collect();
    // 4) Détection des lignes.
    let starts = find_line_starts(&px);
    let height = starts.len();
    if height == 0 {
        return (Vec::new(), 0, 0);
    }

    // 5) Assemblage + collecte pour normalisation.
    let mut rows: Vec<&[f32]> = Vec::with_capacity(height);
    for &s in &starts {
        rows.push(&px[s..s + LINE_PIXELS]);
    }
    let mut all: Vec<f32> = rows.iter().flat_map(|r| r.iter().copied()).collect();
    all.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
    let lo = all[(all.len() as f32 * 0.02) as usize];
    let hi = all[((all.len() as f32 * 0.98) as usize).min(all.len() - 1)];
    let span = (hi - lo).max(1e-6);

    // 6) Quantification 0..255.
    let mut img = vec![0u8; height * LINE_PIXELS];
    for (y, row) in rows.iter().enumerate() {
        for (x, &v) in row.iter().enumerate() {
            img[y * LINE_PIXELS + x] = (((v - lo) / span).clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
    (img, LINE_PIXELS, height)
}

/// Écrit une image niveaux de gris en BMP 24 bits (lisible par Windows).
pub fn write_bmp(path: &str, img: &[u8], w: usize, h: usize) -> io::Result<()> {
    let row_bytes = w * 3;
    let pad = (4 - (row_bytes % 4)) % 4;
    let stride = row_bytes + pad;
    let data_size = stride * h;
    let file_size = 54 + data_size;

    let mut f = File::create(path)?;
    // En-tête fichier (14) + en-tête info BITMAPINFOHEADER (40).
    f.write_all(b"BM")?;
    f.write_all(&(file_size as u32).to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?; // réservé
    f.write_all(&54u32.to_le_bytes())?; // offset des données
    f.write_all(&40u32.to_le_bytes())?; // taille info header
    f.write_all(&(w as i32).to_le_bytes())?;
    f.write_all(&(h as i32).to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // plans
    f.write_all(&24u16.to_le_bytes())?; // bits/pixel
    f.write_all(&0u32.to_le_bytes())?; // compression (BI_RGB)
    f.write_all(&(data_size as u32).to_le_bytes())?;
    f.write_all(&2835i32.to_le_bytes())?; // 72 dpi X
    f.write_all(&2835i32.to_le_bytes())?; // 72 dpi Y
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;

    // Données pixel, de bas en haut (convention BMP).
    let padding = [0u8; 3];
    for y in (0..h).rev() {
        for x in 0..w {
            let v = img[y * w + x];
            f.write_all(&[v, v, v])?;
        }
        f.write_all(&padding[..pad])?;
    }
    Ok(())
}

/// Lit un WAV PCM 16 bits (prend le canal 0). Renvoie (échantillons f32, rate).
pub fn read_wav(path: &str) -> io::Result<(Vec<f32>, f64)> {
    let b = fs::read(path)?;
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "pas un WAV RIFF"));
    }
    let channels = u16::from_le_bytes([b[22], b[23]]) as usize;
    let rate = u32::from_le_bytes([b[24], b[25], b[26], b[27]]) as f64;

    // Cherche le sous-bloc "data".
    let mut pos = 12;
    let (mut data_off, mut data_len) = (0usize, 0usize);
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let sz = u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]) as usize;
        if id == b"data" {
            data_off = pos + 8;
            data_len = sz.min(b.len() - (pos + 8));
            break;
        }
        pos += 8 + sz + (sz & 1);
    }
    if data_off == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bloc data introuvable"));
    }

    let ch = channels.max(1);
    let mut out = Vec::with_capacity(data_len / (2 * ch));
    let mut i = data_off;
    while i + 2 * ch <= data_off + data_len {
        let s = i16::from_le_bytes([b[i], b[i + 1]]); // canal 0
        out.push(f32::from(s) / 32768.0);
        i += 2 * ch;
    }
    Ok((out, rate))
}

/// Génère un audio APT synthétique à partir d'une image (lignes de [`LINE_PIXELS`] octets).
fn synth_apt(image: &[Vec<u8>]) -> Vec<f32> {
    let mut audio = Vec::with_capacity(image.len() * LINE_PIXELS * SAMPLES_PER_PIXEL);
    let dphi = 2.0 * PI * SUBCARRIER / WORK_RATE as f32;
    let mut phase = 0f32;
    for row in image {
        for &p in row {
            let level = f32::from(p) / 255.0;
            let amp = 0.4 + 0.6 * level; // AM (indice ~75 %)
            for _ in 0..SAMPLES_PER_PIXEL {
                audio.push(amp * phase.cos());
                phase += dphi;
                if phase > PI {
                    phase -= 2.0 * PI;
                }
            }
        }
    }
    audio
}

/// Construit une image de test : synchro A + mire (dégradé diagonal).
fn test_image(height: usize) -> Vec<Vec<u8>> {
    let syn = sync_a_ref();
    (0..height)
        .map(|r| {
            let mut row = vec![0u8; LINE_PIXELS];
            for (k, &s) in syn.iter().enumerate() {
                row[k] = if s > 0.0 { 255 } else { 0 };
            }
            for col in syn.len()..LINE_PIXELS {
                // mire sinusoïdale lisse (diagonale) — représentative d'une vraie image
                let v = 128.0 + 90.0 * (2.0 * PI * (col as f32 / 520.0 + r as f32 / 24.0)).sin();
                row[col] = v as u8;
            }
            row
        })
        .collect()
}

/// Auto-test déterministe : synthétise un APT depuis une mire, décode, et
/// mesure la corrélation avec la source (zone image) — sans matériel.
pub fn self_test() -> bool {
    let height = 24;
    let src = test_image(height);
    let audio = synth_apt(&src);
    let (img, w, h) = decode(&audio, WORK_RATE);

    println!("Auto-test décodeur APT :");
    println!("   lignes attendues={height}  détectées={h}  largeur={w}");
    if h == 0 || w != LINE_PIXELS {
        println!("   → ❌ ÉCHEC (aucune ligne détectée)");
        return false;
    }

    // Corrélation entre image décodée et source sur la zone vidéo (hors synchro).
    let cmp_h = h.min(height);
    let (mut sxy, mut sxx, mut syy) = (0f64, 0f64, 0f64);
    let (mut mx, mut my) = (0f64, 0f64);
    let mut count = 0f64;
    let col0 = 60usize;
    for y in 0..cmp_h {
        for x in col0..LINE_PIXELS {
            mx += f64::from(img[y * w + x]);
            my += f64::from(src[y][x]);
            count += 1.0;
        }
    }
    mx /= count;
    my /= count;
    for y in 0..cmp_h {
        for x in col0..LINE_PIXELS {
            let dx = f64::from(img[y * w + x]) - mx;
            let dy = f64::from(src[y][x]) - my;
            sxy += dx * dy;
            sxx += dx * dx;
            syy += dy * dy;
        }
    }
    let corr = sxy / (sxx.sqrt() * syy.sqrt()).max(1e-9);
    println!("   corrélation image décodée/source = {corr:.3} (attendu > 0,9)");

    let _ = write_bmp("apt_selftest.bmp", &img, w, h);
    println!("   (image écrite : apt_selftest.bmp)");

    let ok = h >= height - 2 && corr > 0.9; // 1-2 lignes de bord perdues = normal
    println!("   → {}", if ok { "✅ décodeur APT correct" } else { "❌ ÉCHEC" });
    ok
}
