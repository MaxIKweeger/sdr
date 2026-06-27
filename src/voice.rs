//! Récepteur voix bande étroite (AM aviation / NFM) avec squelch et enregistrement.
//!
//! Chaîne en streaming (état conservé entre buffers, donc pas de clic) :
//! IQ 2,0 MS/s → décimation 2 étages vers ~20 kHz en isolant le canal →
//! démodulation AM (enveloppe) ou NFM (discriminateur) → audio.
//! Un squelch à seuil adaptatif déclenche l'enregistrement quand ça parle.

use crate::fm::lowpass_taps;
use std::f32::consts::PI;

const IQ_RATE: u32 = 2_000_000;
const DECIM1: usize = 20; // 2,0 MS/s → 100 kHz
const IF_RATE: u32 = IQ_RATE / DECIM1 as u32; // 100 kHz
const DECIM2: usize = 5; // 100 kHz → 20 kHz
pub const AUDIO_RATE: u32 = IF_RATE / DECIM2 as u32; // 20 kHz

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Am,
    Nfm,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.to_ascii_lowercase().as_str() {
            "am" => Some(Mode::Am),
            "nfm" | "fm" => Some(Mode::Nfm),
            _ => None,
        }
    }
}

/// Décimateur FIR complexe **en flux** : conserve l'historique entre appels.
struct FirDecim {
    taps: Vec<f32>,
    decim: usize,
    bi: Vec<f32>,
    bq: Vec<f32>,
    next: usize,
}

impl FirDecim {
    fn new(taps: Vec<f32>, decim: usize) -> Self {
        Self { taps, decim, bi: Vec::new(), bq: Vec::new(), next: 0 }
    }

    fn process(&mut self, ini: &[f32], inq: &[f32], oi: &mut Vec<f32>, oq: &mut Vec<f32>) {
        self.bi.extend_from_slice(ini);
        self.bq.extend_from_slice(inq);
        let nt = self.taps.len();
        let mut p = self.next;
        while p + nt <= self.bi.len() {
            let mut si = 0f32;
            let mut sq = 0f32;
            for (k, &t) in self.taps.iter().enumerate() {
                si += t * self.bi[p + k];
                sq += t * self.bq[p + k];
            }
            oi.push(si);
            oq.push(sq);
            p += self.decim;
        }
        let drain = p.min(self.bi.len());
        self.bi.drain(0..drain);
        self.bq.drain(0..drain);
        self.next = p - drain;
    }
}

/// Récepteur voix : 2 étages de décimation + démod, en flux.
pub struct VoiceRx {
    stage1: FirDecim,
    stage2: FirDecim,
    mode: Mode,
    // état AM (blocage du continu / porteuse)
    dc_x1: f32,
    dc_y1: f32,
    // état NFM (discriminateur)
    prev_i: f32,
    prev_q: f32,
    have_prev: bool,
    // filtre voix 300–3400 Hz (passe-haut + passe-bas, état conservé)
    hpf_x1: f32,
    hpf_y1: f32,
    lpf_y1: f32,
}

impl VoiceRx {
    pub fn new(mode: Mode) -> Self {
        // étage 1 : passe-bas large ~30 kHz ; étage 2 : largeur du canal selon le mode.
        let taps1 = lowpass_taps(63, 30_000.0 / IQ_RATE as f32);
        let chan = if mode == Mode::Am { 5_000.0 } else { 8_000.0 };
        let taps2 = lowpass_taps(127, chan / IF_RATE as f32);
        Self {
            stage1: FirDecim::new(taps1, DECIM1),
            stage2: FirDecim::new(taps2, DECIM2),
            mode,
            dc_x1: 0.0,
            dc_y1: 0.0,
            prev_i: 0.0,
            prev_q: 0.0,
            have_prev: false,
            hpf_x1: 0.0,
            hpf_y1: 0.0,
            lpf_y1: 0.0,
        }
    }

    /// Traite un buffer IQ → (audio à [`AUDIO_RATE`], niveau RMS du canal).
    pub fn process(&mut self, iq: &[u8]) -> (Vec<f32>, f32) {
        let n = iq.len() / 2;
        let mut ri = Vec::with_capacity(n);
        let mut rq = Vec::with_capacity(n);
        for c in iq.chunks_exact(2) {
            ri.push(f32::from(c[0]) - 127.5);
            rq.push(f32::from(c[1]) - 127.5);
        }

        let (mut i1, mut q1) = (Vec::new(), Vec::new());
        self.stage1.process(&ri, &rq, &mut i1, &mut q1);
        let (mut i2, mut q2) = (Vec::new(), Vec::new());
        self.stage2.process(&i1, &q1, &mut i2, &mut q2);

        // Niveau du canal (RMS de l'enveloppe) → pour le squelch.
        let mut sumsq = 0f64;
        for k in 0..i2.len() {
            sumsq += f64::from(i2[k] * i2[k] + q2[k] * q2[k]);
        }
        let level = if i2.is_empty() {
            0.0
        } else {
            (sumsq / i2.len() as f64).sqrt() as f32
        };

        // Démodulation.
        let mut audio = Vec::with_capacity(i2.len());
        match self.mode {
            Mode::Am => {
                for k in 0..i2.len() {
                    let mag = (i2[k] * i2[k] + q2[k] * q2[k]).sqrt();
                    // blocage du continu (retire la porteuse) → audio
                    let y = mag - self.dc_x1 + 0.999 * self.dc_y1;
                    self.dc_x1 = mag;
                    self.dc_y1 = y;
                    audio.push(y);
                }
            }
            Mode::Nfm => {
                for k in 0..i2.len() {
                    if self.have_prev {
                        let re = i2[k] * self.prev_i + q2[k] * self.prev_q;
                        let im = q2[k] * self.prev_i - i2[k] * self.prev_q;
                        audio.push(im.atan2(re));
                    }
                    self.prev_i = i2[k];
                    self.prev_q = q2[k];
                    self.have_prev = true;
                }
            }
        }

        // Filtre voix 300–3400 Hz : passe-haut (anti-ronflement) + passe-bas (anti-souffle).
        let r_hp = (-2.0 * PI * 300.0 / AUDIO_RATE as f32).exp();
        let a_lp = 1.0 - (-2.0 * PI * 3400.0 / AUDIO_RATE as f32).exp();
        for s in &mut audio {
            let y = *s - self.hpf_x1 + r_hp * self.hpf_y1;
            self.hpf_x1 = *s;
            self.hpf_y1 = y;
            self.lpf_y1 += a_lp * (y - self.lpf_y1);
            *s = self.lpf_y1;
        }

        (audio, level)
    }
}
