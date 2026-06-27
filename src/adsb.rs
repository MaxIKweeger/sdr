//! Démodulateur ADS-B / Mode S Extended Squitter (1090 MHz), écrit from scratch.
//!
//! Chaîne : IQ 8 bits @ 2,0 MS/s → magnitude → détection du préambule Mode S
//! (8 µs) → démod PPM (112 bits, 2 échantillons/bit) → CRC-24 Mode S → décodage
//! DF17 (ICAO, indicatif, altitude, vitesse).
//!
//! La détection du préambule reprend l'approche de dump1090 (Salvatore Sanfilippo).

use std::collections::HashMap;

const PREAMBLE_US: usize = 8;
const LONG_MSG_BITS: usize = 112;
const LONG_MSG_BYTES: usize = LONG_MSG_BITS / 8; // 14
/// Longueur totale d'un message long (préambule + données) en échantillons @ 2 MS/s.
const FULL_LEN_SAMPLES: usize = (PREAMBLE_US + LONG_MSG_BITS) * 2; // 240

/// Polynôme générateur du CRC-24 Mode S (sans le bit x^24 implicite).
const CRC_POLY: u32 = 0x00ff_f409;

/// Jeu de caractères 6 bits pour les indicatifs (AIS-like).
const AIS_CHARSET: &[u8; 64] =
    b"?ABCDEFGHIJKLMNOPQRSTUVWXYZ????? ???????????????0123456789??????";

/// CRC-24 Mode S, MSB-first. Renvoie le syndrome (0 = trame valide pour DF17/18).
fn modes_crc(msg: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for &b in msg {
        crc ^= u32::from(b) << 16;
        for _ in 0..8 {
            if crc & 0x0080_0000 != 0 {
                crc = (crc << 1) ^ CRC_POLY;
            } else {
                crc <<= 1;
            }
            crc &= 0x00ff_ffff;
        }
    }
    crc & 0x00ff_ffff
}

/// Décode l'indicatif (8 caractères) d'un message d'identification (TC 1-4).
fn decode_callsign(msg: &[u8]) -> String {
    let idx = [
        msg[5] >> 2,
        ((msg[5] & 0x03) << 4) | (msg[6] >> 4),
        ((msg[6] & 0x0f) << 2) | (msg[7] >> 6),
        msg[7] & 0x3f,
        msg[8] >> 2,
        ((msg[8] & 0x03) << 4) | (msg[9] >> 4),
        ((msg[9] & 0x0f) << 2) | (msg[10] >> 6),
        msg[10] & 0x3f,
    ];
    idx.iter()
        .map(|&i| AIS_CHARSET[i as usize] as char)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Décode l'altitude (pieds) d'un message de position aérienne (TC 9-18).
fn decode_altitude(msg: &[u8]) -> Option<i32> {
    let ac12 = ((u32::from(msg[5]) << 4) | (u32::from(msg[6]) >> 4)) & 0x0fff;
    if ac12 & 0x10 != 0 {
        // Q-bit : pas de 25 ft
        let n = ((ac12 & 0x0fe0) >> 1) | (ac12 & 0x000f);
        Some(n as i32 * 25 - 1000)
    } else {
        None // codage Gray (rare), non géré
    }
}

/// Décode la vitesse sol et le cap d'un message de vitesse (TC 19, sous-type 1/2).
fn decode_velocity(msg: &[u8]) -> Option<(f64, f64)> {
    let subtype = msg[4] & 0x07;
    if subtype != 1 && subtype != 2 {
        return None; // vitesse air, non gérée ici
    }
    let mult = if subtype == 2 { 4.0 } else { 1.0 };
    let ew_dir = (msg[5] & 0x04) >> 2;
    let ew_v = (i32::from(msg[5] & 0x03) << 8) | i32::from(msg[6]);
    let ns_dir = (msg[7] & 0x80) >> 7;
    let ns_v = (i32::from(msg[7] & 0x7f) << 3) | (i32::from(msg[8] & 0xe0) >> 5);
    if ew_v == 0 || ns_v == 0 {
        return None;
    }
    let ew = f64::from(ew_v - 1) * mult * if ew_dir == 1 { -1.0 } else { 1.0 };
    let ns = f64::from(ns_v - 1) * mult * if ns_dir == 1 { -1.0 } else { 1.0 };
    let speed = (ew * ew + ns * ns).sqrt();
    let mut heading = ew.atan2(ns).to_degrees();
    if heading < 0.0 {
        heading += 360.0;
    }
    Some((speed, heading))
}

/// Informations accumulées par avion.
#[derive(Default)]
struct Aircraft {
    callsign: Option<String>,
    altitude: Option<i32>,
    speed: Option<f64>,
    heading: Option<f64>,
    messages: u32,
}

/// Résultat d'une tentative de détection à un offset donné.
enum DetectResult {
    None,                          // pas de préambule
    PreambleOnly,                  // préambule plausible mais pas de trame ADS-B valide
    Message([u8; LONG_MSG_BYTES]), // trame DF17/18 valide (CRC OK)
}

pub struct Demod {
    maglut: Vec<u16>,
    aircraft: HashMap<u32, Aircraft>,
    valid_msgs: u64,
    preambles: u64,
}

impl Demod {
    pub fn new() -> Self {
        // Table magnitude : (I,Q) octets → amplitude, échelle ×360 comme dump1090.
        let mut maglut = vec![0u16; 256 * 256];
        for i in 0..256 {
            for q in 0..256 {
                let fi = i as f64 - 127.0;
                let fq = q as f64 - 127.0;
                let mag = ((fi * fi + fq * fq).sqrt() * 360.0).min(65535.0);
                maglut[i * 256 + q] = mag as u16;
            }
        }
        Self { maglut, aircraft: HashMap::new(), valid_msgs: 0, preambles: 0 }
    }

    /// Traite un buffer d'IQ bruts (I,Q entrelacés, u8).
    pub fn process(&mut self, iq: &[u8]) {
        // 1) Magnitude.
        let n = iq.len() / 2;
        let mut m = vec![0u16; n];
        for k in 0..n {
            let i = iq[2 * k] as usize;
            let q = iq[2 * k + 1] as usize;
            m[k] = self.maglut[(i << 8) | q];
        }

        // 2) Recherche de préambules + décodage.
        if m.len() < FULL_LEN_SAMPLES {
            return;
        }
        let mut j = 0;
        while j < m.len() - FULL_LEN_SAMPLES {
            match self.try_decode_at(&m, j) {
                DetectResult::Message(msg) => {
                    self.handle_message(&msg);
                    j += FULL_LEN_SAMPLES; // saute le message décodé
                }
                DetectResult::PreambleOnly => {
                    self.preambles += 1;
                    j += 1;
                }
                DetectResult::None => j += 1,
            }
        }
    }

    /// Tente de détecter un préambule et décoder un message à l'offset `j`.
    fn try_decode_at(&self, m: &[u16], j: usize) -> DetectResult {
        // Forme du préambule Mode S : impulsions aux échantillons 0,2,7,9.
        if !(m[j] > m[j + 1]
            && m[j + 1] < m[j + 2]
            && m[j + 2] > m[j + 3]
            && m[j + 3] < m[j]
            && m[j + 4] < m[j]
            && m[j + 5] < m[j]
            && m[j + 6] < m[j]
            && m[j + 7] > m[j + 8]
            && m[j + 8] < m[j + 9]
            && m[j + 9] > m[j + 6])
        {
            return DetectResult::None;
        }

        // Les impulsions hautes doivent dominer les creux et le début du message.
        let high = (u32::from(m[j]) + u32::from(m[j + 2]) + u32::from(m[j + 7]) + u32::from(m[j + 9])) / 6;
        if u32::from(m[j + 4]) >= high || u32::from(m[j + 5]) >= high {
            return DetectResult::None;
        }
        if u32::from(m[j + 11]) >= high
            || u32::from(m[j + 12]) >= high
            || u32::from(m[j + 13]) >= high
            || u32::from(m[j + 14]) >= high
        {
            return DetectResult::None;
        }

        // Démodulation PPM : 112 bits, 2 échantillons/bit, après les 16 du préambule.
        let mut bits = [0u8; LONG_MSG_BITS];
        for i in 0..LONG_MSG_BITS {
            let base = j + PREAMBLE_US * 2 + i * 2;
            let first = m[base];
            let second = m[base + 1];
            bits[i] = u8::from(first > second); // impulsion 1re moitié = 1
        }

        // Empaquetage en octets.
        let mut msg = [0u8; LONG_MSG_BYTES];
        for i in 0..LONG_MSG_BYTES {
            let b = i * 8;
            msg[i] = (bits[b] << 7)
                | (bits[b + 1] << 6)
                | (bits[b + 2] << 5)
                | (bits[b + 3] << 4)
                | (bits[b + 4] << 3)
                | (bits[b + 5] << 2)
                | (bits[b + 6] << 1)
                | bits[b + 7];
        }

        // On ne garde que DF17/DF18 (ADS-B) avec CRC valide.
        let df = msg[0] >> 3;
        if (df == 17 || df == 18) && modes_crc(&msg) == 0 {
            DetectResult::Message(msg)
        } else {
            DetectResult::PreambleOnly
        }
    }

    fn handle_message(&mut self, msg: &[u8]) {
        self.valid_msgs += 1;
        let icao = (u32::from(msg[1]) << 16) | (u32::from(msg[2]) << 8) | u32::from(msg[3]);
        let tc = msg[4] >> 3;
        let ac = self.aircraft.entry(icao).or_default();
        ac.messages += 1;

        let detail = match tc {
            1..=4 => {
                let cs = decode_callsign(msg);
                ac.callsign = Some(cs.clone());
                format!("indicatif {cs}")
            }
            9..=18 => {
                if let Some(alt) = decode_altitude(msg) {
                    ac.altitude = Some(alt);
                    format!("altitude {alt} ft")
                } else {
                    "position".to_string()
                }
            }
            19 => {
                if let Some((spd, hdg)) = decode_velocity(msg) {
                    ac.speed = Some(spd);
                    ac.heading = Some(hdg);
                    format!("vitesse {spd:.0} kt, cap {hdg:.0}°")
                } else {
                    "vitesse".to_string()
                }
            }
            _ => format!("TC{tc}"),
        };

        println!(
            "✈  {icao:06X}  {:<10}  {detail}",
            self.aircraft[&icao].callsign.as_deref().unwrap_or("")
        );
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    pub fn stats(&self) -> (usize, u64, u64) {
        (self.aircraft.len(), self.valid_msgs, self.preambles)
    }
}

/// Auto-test déterministe : décode un message ADS-B connu (sans matériel).
/// Message : 8D4840D6202CC371C32CE0576098 (avion 4840D6, indicatif « KLM1023 »).
pub fn self_test() -> bool {
    let msg: [u8; 14] = [
        0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98,
    ];
    let crc = modes_crc(&msg);
    let df = msg[0] >> 3;
    let icao = (u32::from(msg[1]) << 16) | (u32::from(msg[2]) << 8) | u32::from(msg[3]);
    let tc = msg[4] >> 3;
    let cs = decode_callsign(&msg);

    println!("Auto-test décodeur (message connu) :");
    println!("   CRC syndrome = 0x{crc:06X} (attendu 0x000000)");
    println!("   DF={df}  ICAO={icao:06X} (attendu 4840D6)  TC={tc}  indicatif=\"{cs}\"");

    let ok = crc == 0 && df == 17 && icao == 0x4840D6 && cs == "KLM1023";
    println!("   → {}", if ok { "✅ décodeur correct" } else { "❌ ÉCHEC" });
    ok
}

/// Auto-test de la chaîne DSP complète : synthétise un buffer IQ contenant le
/// message connu (préambule + PPM) et vérifie qu'il est détecté et décodé.
pub fn synth_test() -> bool {
    let msg: [u8; 14] = [
        0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98,
    ];
    const HIGH: u8 = 227; // forte amplitude sur I
    const LOW: u8 = 137; // amplitude faible

    // Motif d'amplitude sur les 240 échantillons (préambule + 112 bits ×2).
    let mut hi = [false; FULL_LEN_SAMPLES];
    for &p in &[0usize, 2, 7, 9] {
        hi[p] = true; // impulsions du préambule
    }
    for i in 0..LONG_MSG_BITS {
        let bit = (msg[i / 8] >> (7 - (i % 8))) & 1;
        let base = PREAMBLE_US * 2 + i * 2;
        // PPM : impulsion 1re moitié = 1, 2e moitié = 0
        if bit == 1 {
            hi[base] = true;
        } else {
            hi[base + 1] = true;
        }
    }

    // Construit le buffer IQ (Q fixe à 127, I porte l'amplitude), avec marge.
    let mut iq = Vec::new();
    let mut push = |high: bool| {
        iq.push(if high { HIGH } else { LOW });
        iq.push(127u8);
    };
    for _ in 0..20 {
        push(false);
    }
    for &h in &hi {
        push(h);
    }
    for _ in 0..20 {
        push(false);
    }

    let mut demod = Demod::new();
    demod.process(&iq);
    let (_, msgs, _) = demod.stats();
    println!("Auto-test chaîne DSP (IQ synthétique) : {msgs} message(s) décodé(s)");
    println!("   → {}", if msgs >= 1 { "✅ chaîne IQ→message correcte" } else { "❌ ÉCHEC" });
    msgs >= 1
}
