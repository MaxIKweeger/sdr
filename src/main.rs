//! sdr.exe — étape 3b : accorder réellement le tuner R820T2 et mesurer la puissance.
//!
//! On pilote le RTL2832U via des transferts de contrôle USB (comme librtlsdr) et
//! le tuner R820T2 en I2C (module `r820t`). Vérification : un balayage de puissance
//! montre la bande FM (~88-108 MHz) bien au-dessus du plancher de bruit, prouvant
//! que l'accord fonctionne. Avec un argument `<MHz>`, on mesure une seule fréquence.

mod adsb;
mod apt;
mod fm;
mod predict;
mod r820t;
mod satdump;
mod voice;

use std::error::Error;
use std::time::Duration;

use nusb::transfer::{Control, ControlType, Recipient, RequestBuffer};
use nusb::Interface;

use r820t::R820t2;

const RTL_VENDOR_ID: u16 = 0x0bda;
const RTL_PRODUCT_IDS: &[(u16, &str)] = &[
    (0x2832, "Generic RTL2832U"),
    (0x2838, "RTL2838 DVB-T (NESDR SMArt / R820T2)"),
    (0x2831, "RTL2831U"),
    (0x2837, "RTL2832U variant"),
];

// --- Blocs d'adressage du RTL2832U ---
const USBB: u16 = 1;
const SYSB: u16 = 2;
const IICB: u16 = 6;

// --- Registres ---
const USB_SYSCTL: u16 = 0x2000;
const USB_EPA_CTL: u16 = 0x2148;
const USB_EPA_MAXPKT: u16 = 0x2158;
const DEMOD_CTL: u16 = 0x3000;
const DEMOD_CTL_1: u16 = 0x300b;

const R82XX_CHECK_VAL: u8 = 0x69;

// --- Échantillonnage ---
const RTL_XTAL: u32 = 28_800_000;
const BULK_ENDPOINT: u8 = 0x81;
const SAMPLE_RATE: u32 = 2_000_000;
const SCAN_GAIN_TENTH_DB: i32 = 240; // ~24 dB, modéré pour le balayage
const FM_GAIN_TENTH_DB: i32 = 300; // ~30 dB pour la FM (évite la saturation ADC)
const ADSB_GAIN_TENTH_DB: i32 = 496; // ~max, pour la sensibilité ADS-B
const NOAA_GAIN_TENTH_DB: i32 = 420; // ~42 dB pour un satellite (signal faible)
const VOICE_GAIN_TENTH_DB: i32 = 380; // ~38 dB pour la voix (aviation faible sur cette antenne)
const ADSB_FREQ: u32 = 1_090_000_000;

const FIR_DEFAULT: [i32; 16] = [
    -54, -36, -41, -40, -32, -14, 14, 53, 101, 156, 215, 273, 327, 372, 404, 421,
];

const CTRL_TIMEOUT: Duration = Duration::from_millis(300);

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Enveloppe l'interface USB claimée et expose les accès registres du RTL2832U.
pub struct RtlSdr {
    iface: Interface,
}

impl RtlSdr {
    fn ctrl(value: u16, index: u16) -> Control {
        Control {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request: 0,
            value,
            index,
        }
    }

    // --- Registres « plats » (USB, SYS) ---

    #[allow(dead_code)]
    fn read_reg(&self, block: u16, addr: u16, len: usize) -> Result<u16> {
        let mut data = [0u8; 2];
        self.iface
            .control_in_blocking(Self::ctrl(addr, block << 8), &mut data[..len], CTRL_TIMEOUT)?;
        Ok((u16::from(data[1]) << 8) | u16::from(data[0]))
    }

    fn write_reg(&self, block: u16, addr: u16, val: u16, len: usize) -> Result<()> {
        let mut data = [0u8; 2];
        data[0] = if len == 1 { (val & 0xff) as u8 } else { (val >> 8) as u8 };
        data[1] = (val & 0xff) as u8;
        self.iface
            .control_out_blocking(Self::ctrl(addr, (block << 8) | 0x10), &data[..len], CTRL_TIMEOUT)?;
        Ok(())
    }

    // --- Registres du démodulateur (pages) ---

    fn demod_read_reg(&self, page: u16, addr: u16, len: usize) -> Result<u16> {
        let mut data = [0u8; 2];
        let real_addr = (addr << 8) | 0x20;
        self.iface
            .control_in_blocking(Self::ctrl(real_addr, page), &mut data[..len], CTRL_TIMEOUT)?;
        Ok((u16::from(data[1]) << 8) | u16::from(data[0]))
    }

    fn demod_write_reg(&self, page: u16, addr: u16, val: u16, len: usize) -> Result<()> {
        let mut data = [0u8; 2];
        data[0] = if len == 1 { (val & 0xff) as u8 } else { (val >> 8) as u8 };
        data[1] = (val & 0xff) as u8;
        let real_addr = (addr << 8) | 0x20;
        self.iface
            .control_out_blocking(Self::ctrl(real_addr, 0x10 | page), &data[..len], CTRL_TIMEOUT)?;
        self.demod_read_reg(0x0a, 0x01, 1)?; // cycle de validation
        Ok(())
    }

    // --- Bus I2C (vers le tuner) ---

    /// Écriture I2C : `[reg, vals...]`, découpée en blocs de 7 octets (max_i2c_msg_len = 8).
    fn i2c_write(&self, i2c_addr: u8, reg: u8, vals: &[u8]) -> Result<()> {
        let addr = u16::from(i2c_addr);
        let mut reg = reg;
        let mut pos = 0;
        while pos < vals.len() {
            let size = (vals.len() - pos).min(7);
            let mut buf = Vec::with_capacity(size + 1);
            buf.push(reg);
            buf.extend_from_slice(&vals[pos..pos + size]);
            self.iface
                .control_out_blocking(Self::ctrl(addr, (IICB << 8) | 0x10), &buf, CTRL_TIMEOUT)?;
            reg = reg.wrapping_add(size as u8);
            pos += size;
        }
        Ok(())
    }

    /// Lecture I2C : pointe `reg` puis lit `buf.len()` octets.
    fn i2c_read(&self, i2c_addr: u8, reg: u8, buf: &mut [u8]) -> Result<()> {
        let addr = u16::from(i2c_addr);
        self.iface
            .control_out_blocking(Self::ctrl(addr, (IICB << 8) | 0x10), &[reg], CTRL_TIMEOUT)?;
        self.iface
            .control_in_blocking(Self::ctrl(addr, IICB << 8), buf, CTRL_TIMEOUT)?;
        Ok(())
    }

    fn i2c_read_reg(&self, i2c_addr: u8, reg: u8) -> Result<u8> {
        let mut data = [0u8; 1];
        self.i2c_read(i2c_addr, reg, &mut data)?;
        Ok(data[0])
    }

    fn set_i2c_repeater(&self, on: bool) -> Result<()> {
        self.demod_write_reg(1, 0x01, if on { 0x18 } else { 0x10 }, 1)
    }

    // --- Init / IF ---

    fn set_fir(&self) -> Result<()> {
        let mut fir = [0u8; 20];
        for i in 0..8 {
            fir[i] = (FIR_DEFAULT[i] as i8) as u8;
        }
        let mut i = 0;
        while i < 8 {
            let val0 = FIR_DEFAULT[8 + i];
            let val1 = FIR_DEFAULT[8 + i + 1];
            fir[8 + i * 3 / 2] = (val0 >> 4) as u8;
            fir[8 + i * 3 / 2 + 1] = ((val0 << 4) | ((val1 >> 8) & 0x0f)) as u8;
            fir[8 + i * 3 / 2 + 2] = val1 as u8;
            i += 2;
        }
        for (k, b) in fir.iter().enumerate() {
            self.demod_write_reg(1, 0x1c + k as u16, u16::from(*b), 1)?;
        }
        Ok(())
    }

    fn init_baseband(&self) -> Result<()> {
        self.write_reg(USBB, USB_SYSCTL, 0x09, 1)?;
        self.write_reg(USBB, USB_EPA_MAXPKT, 0x0002, 2)?;
        self.write_reg(USBB, USB_EPA_CTL, 0x1002, 2)?;
        self.write_reg(SYSB, DEMOD_CTL_1, 0x22, 1)?;
        self.write_reg(SYSB, DEMOD_CTL, 0xe8, 1)?;
        self.demod_write_reg(1, 0x01, 0x14, 1)?;
        self.demod_write_reg(1, 0x01, 0x10, 1)?;
        self.demod_write_reg(1, 0x15, 0x00, 1)?;
        self.demod_write_reg(1, 0x16, 0x0000, 2)?;
        for i in 0..6 {
            self.demod_write_reg(1, 0x16 + i, 0x00, 1)?;
        }
        self.set_fir()?;
        self.demod_write_reg(0, 0x19, 0x05, 1)?;
        self.demod_write_reg(1, 0x93, 0xf0, 1)?;
        self.demod_write_reg(1, 0x94, 0x0f, 1)?;
        self.demod_write_reg(1, 0x11, 0x00, 1)?;
        self.demod_write_reg(1, 0x04, 0x00, 1)?;
        self.demod_write_reg(0, 0x61, 0x60, 1)?;
        self.demod_write_reg(0, 0x06, 0x80, 1)?;
        self.demod_write_reg(1, 0xb1, 0x1b, 1)?;
        self.demod_write_reg(0, 0x0d, 0x83, 1)?;
        Ok(())
    }

    /// Programme l'IF du démodulateur (port de rtlsdr_set_if_freq, calcul en f64).
    fn set_if_freq(&self, freq: u32) -> Result<()> {
        let if_freq = (-((f64::from(freq) * 4_194_304.0) / f64::from(RTL_XTAL))) as i32;
        self.demod_write_reg(1, 0x19, ((if_freq >> 16) & 0x3f) as u16, 1)?;
        self.demod_write_reg(1, 0x1a, ((if_freq >> 8) & 0xff) as u16, 1)?;
        self.demod_write_reg(1, 0x1b, (if_freq & 0xff) as u16, 1)?;
        Ok(())
    }

    /// Config démod spécifique R820T : IF basse 3,57 MHz (pas de zero-IF).
    fn config_demod_for_r820t(&self) -> Result<()> {
        self.demod_write_reg(1, 0xb1, 0x1a, 1)?; // désactive zero-IF
        self.demod_write_reg(0, 0x08, 0x4d, 1)?; // seule l'entrée ADC I
        self.set_if_freq(r820t::IF_FREQ)?; // 3,57 MHz
        self.demod_write_reg(1, 0x15, 0x01, 1)?; // inversion de spectre
        Ok(())
    }

    fn set_sample_rate(&self, rate: u32) -> Result<u32> {
        let base = u64::from(RTL_XTAL) << 22;
        let mut rsamp_ratio = (base / u64::from(rate)) as u32;
        rsamp_ratio &= 0x0fff_fffc;
        let real_rate = (base / u64::from(rsamp_ratio)) as u32;
        self.demod_write_reg(1, 0x9f, ((rsamp_ratio >> 16) & 0xffff) as u16, 2)?;
        self.demod_write_reg(1, 0xa1, (rsamp_ratio & 0xffff) as u16, 2)?;
        self.demod_write_reg(1, 0x01, 0x14, 1)?;
        self.demod_write_reg(1, 0x01, 0x10, 1)?;
        Ok(real_rate)
    }

    fn reset_buffer(&self) -> Result<()> {
        self.write_reg(USBB, USB_EPA_CTL, 0x1002, 2)?;
        self.write_reg(USBB, USB_EPA_CTL, 0x0000, 2)?;
        Ok(())
    }

    fn read_samples(&self, len: usize) -> Result<Vec<u8>> {
        let completion =
            pollster::block_on(self.iface.bulk_in(BULK_ENDPOINT, RequestBuffer::new(len)));
        completion.status?;
        Ok(completion.data)
    }
}

/// Accorde une fréquence et mesure la puissance RMS du signal IQ.
/// Renvoie (rms, dBFS, fraction saturée).
fn measure_power(sdr: &RtlSdr, tuner: &mut R820t2, freq_hz: u32) -> Result<(f64, f64, f64)> {
    sdr.set_i2c_repeater(true)?;
    tuner.set_freq(sdr, freq_hz)?;
    sdr.set_i2c_repeater(false)?;

    sdr.reset_buffer()?;
    let _ = sdr.read_samples(128 * 1024)?; // jette le buffer périmé après retune
    let buf = sdr.read_samples(128 * 1024)?;

    let mut sumsq = 0f64;
    let mut sat = 0usize;
    let pairs = buf.len() / 2;
    for c in buf.chunks_exact(2) {
        let i = f64::from(c[0]) - 127.5;
        let q = f64::from(c[1]) - 127.5;
        sumsq += i * i + q * q;
        if c[0] == 0 || c[0] == 255 || c[1] == 0 || c[1] == 255 {
            sat += 1;
        }
    }
    let rms = (sumsq / pairs as f64).sqrt();
    let dbfs = 20.0 * (rms / 127.5).log10();
    Ok((rms, dbfs, sat as f64 / pairs as f64))
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("\n❌ Erreur : {e}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let arg = std::env::args().nth(1);

    // Mode `test` : auto-test du décodeur, sans matériel.
    if arg.as_deref() == Some("test") {
        adsb::self_test();
        println!();
        adsb::synth_test();
        return Ok(());
    }

    // Mode `passes` : prédiction des passages, sans matériel.
    if arg.as_deref() == Some("passes") {
        predict::locate_via_gps(); // position via GPS (sinon défaut Bagneux)
        let sats = predict::fetch_sats()?;
        let now = predict::now_unix();
        println!("\nPassages NOAA/Meteor (48h) :\n");
        let passes = predict::find_passes(&sats, now, now + 48.0 * 3600.0);
        predict::print_passes(&passes);
        return Ok(());
    }

    // Mode `apt` : décodeur d'images NOAA, sans matériel.
    if arg.as_deref() == Some("apt") {
        match std::env::args().nth(2).as_deref() {
            Some("test") | None => {
                apt::self_test();
            }
            Some(file) => {
                let (samples, rate) = apt::read_wav(file)?;
                println!("WAV {file} : {} échantillons @ {rate} Hz", samples.len());
                let (img, w, h) = apt::decode(&samples, rate);
                if h == 0 {
                    return Err("aucune ligne APT détectée dans ce WAV".into());
                }
                apt::write_bmp("apt.bmp", &img, w, h)?;
                println!("✅ Image {w}×{h} → apt.bmp");
            }
        }
        return Ok(());
    }

    let scan_mode = arg.as_deref() == Some("scan");
    let fm_mode = arg.as_deref() == Some("fm");
    let noaa_mode = arg.as_deref() == Some("noaa");
    let meteor_mode = arg.as_deref() == Some("meteor");
    let auto_mode = arg.as_deref() == Some("auto");
    let adsb_mode = arg.as_deref() == Some("adsb");
    let listen_mode = arg.as_deref() == Some("listen");
    let fm_freq: Option<f64> = if fm_mode {
        std::env::args().nth(2).and_then(|s| s.parse().ok())
    } else {
        None
    };
    let fm_hp: f32 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(60.0);
    let single_freq: Option<f64> = arg.as_deref().and_then(|s| s.parse().ok());
    let measure_mode = scan_mode || single_freq.is_some();

    // Sans commande matérielle reconnue → affiche l'aide (plus d'ADS-B par défaut).
    if !(scan_mode
        || fm_mode
        || noaa_mode
        || meteor_mode
        || auto_mode
        || adsb_mode
        || listen_mode
        || single_freq.is_some())
    {
        print_usage();
        return Ok(());
    }

    // Gain selon le mode.
    let gain = if fm_mode {
        FM_GAIN_TENTH_DB
    } else if noaa_mode || meteor_mode || auto_mode {
        NOAA_GAIN_TENTH_DB
    } else if listen_mode {
        VOICE_GAIN_TENTH_DB
    } else if measure_mode {
        SCAN_GAIN_TENTH_DB
    } else {
        ADSB_GAIN_TENTH_DB
    };

    let info = nusb::list_devices()?
        .find(|d| {
            d.vendor_id() == RTL_VENDOR_ID
                && RTL_PRODUCT_IDS.iter().any(|(p, _)| *p == d.product_id())
        })
        .ok_or("Aucun module RTL-SDR connu détecté")?;

    println!(
        "✅ Module : USB {:04x}:{:04x} ({})",
        info.vendor_id(),
        info.product_id(),
        info.product_string().unwrap_or("?"),
    );

    let dev = info.open()?;
    let iface = dev.claim_interface(0)?;
    let sdr = RtlSdr { iface };

    // Init RTL2832U.
    sdr.init_baseband()?;

    // Init tuner R820T2 (repeater I2C actif).
    let mut tuner = R820t2::new();
    sdr.set_i2c_repeater(true)?;
    let tuner_id = sdr.i2c_read_reg(r820t::R820T_I2C_ADDR, 0x00)?;
    if tuner_id != R82XX_CHECK_VAL {
        sdr.set_i2c_repeater(false)?;
        return Err(format!("ID tuner inattendu : 0x{tuner_id:02x}").into());
    }
    tuner.init(&sdr)?;
    tuner.set_gain_manual(&sdr, gain)?;
    sdr.set_i2c_repeater(false)?;
    println!("✅ Tuner R820T2 initialisé, gain ~{:.1} dB", gain as f64 / 10.0);

    // Config démod + sample rate.
    sdr.config_demod_for_r820t()?;
    let real_rate = sdr.set_sample_rate(SAMPLE_RATE)?;
    println!("✅ Sample rate : {real_rate} S/s\n");

    if let Some(mhz) = single_freq {
        let (rms, dbfs, sat) = measure_power(&sdr, &mut tuner, (mhz * 1e6) as u32)?;
        println!("Fréquence {mhz:.3} MHz :");
        println!("   RMS {rms:6.2}   {dbfs:6.1} dBFS   saturation {:.1} %", sat * 100.0);
        return Ok(());
    }

    if scan_mode {
        return power_scan(&sdr, &mut tuner);
    }

    if fm_mode {
        return run_fm(&sdr, &mut tuner, fm_freq, fm_hp);
    }

    if noaa_mode {
        let freq_mhz = std::env::args()
            .nth(2)
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or("usage : sdr noaa <freq_MHz> [secondes]  (ex. 137.1 pour NOAA-19)")?;
        let seconds = std::env::args()
            .nth(3)
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(40);
        return run_noaa(&sdr, &mut tuner, freq_mhz, seconds);
    }

    if meteor_mode {
        let freq_mhz = std::env::args()
            .nth(2)
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or("usage : sdr meteor <freq_MHz> [secondes]  (ex. 137.9 pour Meteor-M2-3)")?;
        let seconds = std::env::args()
            .nth(3)
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(600);
        return run_meteor(&sdr, &mut tuner, freq_mhz, seconds);
    }

    if auto_mode {
        return run_auto(&sdr, &mut tuner);
    }

    if listen_mode {
        let freq_mhz = std::env::args()
            .nth(2)
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or("usage : sdr listen <MHz> [am|nfm] [squelch]  (ex. 118.7 am)")?;
        let mode = std::env::args()
            .nth(3)
            .and_then(|s| voice::Mode::parse(&s))
            .unwrap_or(voice::Mode::Am);
        let squelch = std::env::args()
            .nth(4)
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(4.0);
        return run_listen(&sdr, &mut tuner, freq_mhz, mode, squelch);
    }

    run_adsb(&sdr, &mut tuner)
}

/// Récepteur voix (AM/NFM) : écoute une fréquence et enregistre l'activité.
fn run_listen(
    sdr: &RtlSdr,
    tuner: &mut R820t2,
    freq_mhz: f64,
    mode: voice::Mode,
    squelch_factor: f32,
) -> Result<()> {
    std::fs::create_dir_all("recordings")?;
    let center = (freq_mhz * 1e6) as u32;
    sdr.set_i2c_repeater(true)?;
    tuner.set_freq(sdr, center)?;
    sdr.set_i2c_repeater(false)?;
    sdr.reset_buffer()?;
    let _ = sdr.read_samples(256 * 1024)?;

    let modestr = match mode {
        voice::Mode::Am => "AM",
        voice::Mode::Nfm => "NFM",
    };
    println!(
        "🎧 Écoute {freq_mhz:.4} MHz ({modestr}) — squelch ×{squelch_factor:.1}. Ctrl+C pour arrêter.\n   \
         (les transmissions sont enregistrées dans recordings/)"
    );

    const HANG_CHUNKS: i32 = 8; // ~1 s de maintien après la fin de parole
    let mut rx = voice::VoiceRx::new(mode);
    let mut noise = -1f32;
    let mut hang = 0i32;
    let mut clip: Vec<f32> = Vec::new();
    let mut clip_start = chrono::Local::now();
    let mut ctr = 0u32;

    loop {
        let iq = sdr.read_samples(512 * 1024)?; // ~131 ms
        let (audio, level) = rx.process(&iq);
        if noise < 0.0 {
            noise = level.max(0.01);
        }
        let open = level > noise * squelch_factor && level > 0.6;
        if open {
            hang = HANG_CHUNKS;
        } else if hang > 0 {
            hang -= 1;
        }

        if hang > 0 {
            if clip.is_empty() {
                clip_start = chrono::Local::now();
                eprint!("\r🔊 transmission…                                  ");
            }
            clip.extend_from_slice(&audio);
        } else {
            if !clip.is_empty() {
                save_clip(&clip, voice::AUDIO_RATE, freq_mhz, clip_start)?;
                clip.clear();
            }
            noise = 0.97 * noise + 0.03 * level; // suit le bruit de fond au silence
            ctr += 1;
            if ctr % 8 == 0 {
                eprint!(
                    "\r   silence — niveau {level:5.1}  seuil {:5.1}            ",
                    noise * squelch_factor
                );
            }
        }
    }
}

/// Normalise et écrit un clip audio enregistré.
fn save_clip(clip: &[f32], rate: u32, freq_mhz: f64, start: chrono::DateTime<chrono::Local>) -> Result<()> {
    if clip.len() < (rate as usize) / 2 {
        return Ok(()); // < 0,5 s : sûrement un parasite, on ignore
    }
    let peak = clip.iter().fold(1e-9f32, |m, &v| m.max(v.abs()));
    let g = 0.9 * 32767.0 / peak;
    let pcm: Vec<i16> = clip.iter().map(|&v| (v * g).clamp(-32767.0, 32767.0) as i16).collect();
    let path = format!("recordings/{freq_mhz:.3}MHz_{}.wav", start.format("%Y%m%d_%H%M%S"));
    fm::write_wav(&path, &pcm, rate)?;
    eprintln!(
        "\r✅ {path}  ({:.1} s)                              ",
        clip.len() as f64 / rate as f64
    );
    Ok(())
}

fn print_usage() {
    println!("sdr — récepteur SDR (RTL-SDR) tout-en-Rust\n");
    println!("Usage : sdr <commande>\n");
    println!("  (aucun)            cette aide");
    println!("  auto               🛰️  AUTONOME : capture chaque bon passage NOAA/Meteor → passages/");
    println!("  passes             liste les passages satellites (48 h) au-dessus de Bagneux");
    println!("  (position : lue automatiquement via le GPS USB sur {} ; défaut Bagneux si absent)", "/dev/ttyACM0");
    println!("  noaa <MHz> [s]     capture manuelle d'un passage NOAA → noaa.bmp");
    println!("  meteor <MHz> [s]   capture Meteor-M (LRPT numérique) → meteo/ (via SatDump)");
    println!("  apt <fichier.wav>  décode un enregistrement APT → apt.bmp");
    println!("  apt test           auto-test du décodeur APT");
    println!("  fm [MHz] [hp_Hz]   radio FM → fm.wav  (sans MHz : cherche une station)");
    println!("  listen <MHz> [am|nfm]  écoute voix + enregistre l'activité → recordings/");
    println!("  adsb               récepteur ADS-B (avions) en direct");
    println!("  scan               balayage de puissance (vérif tuner)");
    println!("  test               auto-tests du décodeur ADS-B");
    println!("  <MHz>              mesure de puissance à une fréquence");
}

/// Fréquence centrale d'accord couvrant toute la bande APT NOAA (137,1–137,9125)
/// dans les 2 MHz échantillonnés → permet de décoder plusieurs sats à la fois.
const NOAA_BAND_CENTER: u32 = 137_500_000;

/// Mode autonome : capture large bande du 137 MHz et décode TOUS les satellites
/// présents dans la fenêtre (capture parallèle avec une seule clé).
fn run_auto(sdr: &RtlSdr, tuner: &mut R820t2) -> Result<()> {
    const TRIGGER_EL: f64 = 20.0; // élévation min pour déclencher une capture
    const GROUP_EL: f64 = 8.0; // on décode aussi les sats plus bas présents en même temps
    std::fs::create_dir_all("passages")?;
    predict::locate_via_gps(); // position via GPS (sinon défaut Bagneux)
    println!("🛰️  Mode autonome — capture large bande 137 MHz, multi-satellites → passages/");
    println!("    (laisse tourner ; Ctrl+C pour arrêter)\n");

    loop {
        let sats = match predict::fetch_sats() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("⚠️  TLE indisponibles ({e}) — nouvelle tentative dans 10 min");
                std::thread::sleep(std::time::Duration::from_secs(600));
                continue;
            }
        };
        let now = predict::now_unix();
        let all = predict::find_passes(&sats, now, now + 24.0 * 3600.0);

        // Passage déclencheur : prochain NOAA décodable, élévation suffisante.
        let Some(trigger) = all
            .iter()
            .find(|p| p.decodable && p.max_el >= TRIGGER_EL && p.aos > now + 20.0)
        else {
            eprintln!("Aucun bon passage NOAA dans les 24 h — réessai dans 1 h");
            std::thread::sleep(std::time::Duration::from_secs(3600));
            continue;
        };

        // Fenêtre = union des passages décodables qui se chevauchent.
        let (mut start, mut end) = (trigger.aos, trigger.los);
        for _ in 0..2 {
            for p in &all {
                if p.decodable && p.max_el >= GROUP_EL && p.aos < end && p.los > start {
                    start = start.min(p.aos);
                    end = end.max(p.los);
                }
            }
        }
        let group: Vec<&predict::Pass> = all
            .iter()
            .filter(|p| p.decodable && p.max_el >= GROUP_EL && p.aos < end && p.los > start)
            .collect();

        use chrono::{Local, TimeZone};
        println!(
            "⏭️  Fenêtre {} → {} : {} satellite(s)",
            Local.timestamp_opt(start as i64, 0).single().unwrap().format("%a %d/%m %H:%M"),
            Local.timestamp_opt(end as i64, 0).single().unwrap().format("%H:%M"),
            group.len()
        );
        for p in &group {
            println!("     {} (élév max {:.0}°, {:.3} MHz)", p.name, p.max_el, p.freq_mhz);
        }

        // Attente jusqu'au début de la fenêtre.
        loop {
            let rem = start - predict::now_unix();
            if rem <= 0.0 {
                break;
            }
            eprint!("\r    départ dans {rem:>5.0} s…    ");
            std::thread::sleep(std::time::Duration::from_secs_f64(rem.min(15.0)));
        }
        eprintln!();

        if let Err(e) = capture_band(sdr, tuner, end, &group) {
            eprintln!("⚠️  Capture échouée : {e}");
        }
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
}

/// Capture la bande 137 MHz jusqu'à `end`, puis décode chaque satellite du groupe
/// depuis le même enregistrement (décalage numérique par satellite).
fn capture_band(sdr: &RtlSdr, tuner: &mut R820t2, end: f64, group: &[&predict::Pass]) -> Result<()> {
    sdr.set_i2c_repeater(true)?;
    tuner.set_freq(sdr, NOAA_BAND_CENTER)?;
    sdr.set_i2c_repeater(false)?;
    sdr.reset_buffer()?;
    let _ = sdr.read_samples(256 * 1024)?;

    println!(
        "📡 Capture large bande {:.1} MHz jusqu'à la fin de la fenêtre…",
        NOAA_BAND_CENTER as f64 / 1e6
    );
    let remaining = (end - predict::now_unix()).max(1.0);
    let mut iq = Vec::with_capacity((remaining * SAMPLE_RATE as f64 * 2.0) as usize);
    let mut last = 0u64;
    while predict::now_unix() < end {
        let b = sdr.read_samples(256 * 1024)?;
        iq.extend_from_slice(&b);
        let secs = (iq.len() / (SAMPLE_RATE as usize * 2)) as u64;
        if secs != last {
            last = secs;
            eprint!("\r   {secs} s capturés ({} Mo)   ", iq.len() / 1_000_000);
        }
    }
    eprintln!();

    let stamp = chrono::Local::now().format("%Y%m%d_%H%M");

    // Le LRPT Meteor passe par SatDump : on écrit le baseband brut une seule fois
    // (réutilisé par chaque satellite Meteor avec son propre décalage).
    let needs_satdump = group.iter().any(|p| p.name.starts_with("Meteor"));
    let bb_path = format!("passages/baseband_{stamp}.cu8");
    let mut have_satdump = false;
    if needs_satdump {
        have_satdump = satdump::available();
        if have_satdump {
            std::fs::write(&bb_path, &iq)?;
        } else {
            eprintln!(
                "   ⚠️  SatDump introuvable — passage(s) Meteor (LRPT) ignoré(s). \
                 Installe SatDump ou définis SATDUMP_BIN."
            );
        }
    }

    for p in group {
        let offset = p.freq_mhz * 1e6 - NOAA_BAND_CENTER as f64;
        let safe: String = p.name.chars().filter(|c| !c.is_whitespace()).collect();

        // Meteor → LRPT numérique via SatDump.
        if let Some(pipeline) = satdump::pipeline_for(&p.name).filter(|_| p.name.starts_with("Meteor")) {
            if !have_satdump {
                continue;
            }
            let out = format!("passages/{safe}_{stamp}_el{:.0}", p.max_el);
            println!("   décodage {} via SatDump (LRPT, décalage {:+.0} kHz)…", p.name, offset / 1000.0);
            match satdump::decode_file(
                pipeline,
                std::path::Path::new(&bb_path),
                SAMPLE_RATE,
                offset,
                std::path::Path::new(&out),
            ) {
                Ok(()) => println!("   ✅ {out}/  (images SatDump)"),
                Err(e) => eprintln!("   ⚠️  {} : SatDump a échoué : {e}", p.name),
            }
            continue;
        }

        // NOAA → APT analogique, décodeur Rust.
        println!("   décodage {} (APT Rust, décalage {:+.0} kHz)…", p.name, offset / 1000.0);
        let (audio, rate) = fm::demod_apt_shifted(&iq, offset);
        let (img, w, h) = apt::decode(&audio, rate);
        let path = format!("passages/{safe}_{stamp}_el{:.0}.bmp", p.max_el);
        if h == 0 {
            eprintln!("   ⚠️  {} : aucune ligne (signal trop faible)", p.name);
            continue;
        }
        apt::write_bmp(&path, &img, w, h)?;
        let note = if h < 100 { " ⚠️ peu de lignes" } else { "" };
        println!("   ✅ {path}  ({w}×{h}){note}");
    }

    // Le baseband brut est volumineux (~240 Mo/min) : on le supprime après décodage.
    if have_satdump {
        let _ = std::fs::remove_file(&bb_path);
    }
    Ok(())
}

/// Capture un passage NOAA, démodule l'APT et reconstruit l'image.
fn run_noaa(sdr: &RtlSdr, tuner: &mut R820t2, freq_mhz: f64, seconds: u32) -> Result<()> {
    let center = (freq_mhz * 1e6) as u32;
    sdr.set_i2c_repeater(true)?;
    tuner.set_freq(sdr, center)?;
    sdr.set_i2c_repeater(false)?;
    sdr.reset_buffer()?;
    let _ = sdr.read_samples(256 * 1024)?; // jette le buffer périmé

    println!(
        "📡 Capture NOAA sur {:.4} MHz pendant {seconds} s… (vise le pic du passage)",
        freq_mhz
    );
    let want = seconds as usize * SAMPLE_RATE as usize * 2;
    let mut iq = Vec::with_capacity(want);
    let mut last = 0usize;
    while iq.len() < want {
        let b = sdr.read_samples(256 * 1024)?;
        iq.extend_from_slice(&b);
        let secs = iq.len() / (SAMPLE_RATE as usize * 2);
        if secs != last {
            last = secs;
            eprint!("\r   {secs}/{seconds} s capturés ({} Mo)   ", iq.len() / 1_000_000);
        }
    }
    eprintln!();

    println!("   démodulation FM bande étroite…");
    let (audio, rate) = fm::demod_apt(&iq);

    // Archive l'audio APT en WAV (pour re-décoder/ajuster plus tard).
    let peak = audio.iter().fold(1e-9f32, |m, &v| m.max(v.abs()));
    let wav: Vec<i16> = audio
        .iter()
        .map(|&v| (v / peak * 30000.0) as i16)
        .collect();
    fm::write_wav("noaa.wav", &wav, rate as u32)?;

    println!("   décodage APT…");
    let (img, w, h) = apt::decode(&audio, rate);
    if h == 0 {
        return Err("aucune ligne APT détectée (signal trop faible ou pas de passage ?)".into());
    }
    apt::write_bmp("noaa.bmp", &img, w, h)?;
    println!(
        "✅ Image {w}×{h} → noaa.bmp   (audio archivé → noaa.wav)\n   Ouvre-la :  start noaa.bmp"
    );
    Ok(())
}

/// Capture un passage Meteor-M et décode le LRPT (numérique) via SatDump.
/// La capture est centrée directement sur le satellite (donc `freq_shift` = 0).
fn run_meteor(sdr: &RtlSdr, tuner: &mut R820t2, freq_mhz: f64, seconds: u32) -> Result<()> {
    if !satdump::available() {
        return Err(
            "SatDump introuvable : installe-le et ajoute-le au PATH, ou définis SATDUMP_BIN \
             (chemin vers satdump.exe)."
                .into(),
        );
    }
    let center = (freq_mhz * 1e6) as u32;
    sdr.set_i2c_repeater(true)?;
    tuner.set_freq(sdr, center)?;
    sdr.set_i2c_repeater(false)?;
    sdr.reset_buffer()?;
    let _ = sdr.read_samples(256 * 1024)?; // jette le buffer périmé

    println!(
        "📡 Capture Meteor LRPT sur {freq_mhz:.4} MHz pendant {seconds} s… (vise tout le passage)"
    );
    let want = seconds as usize * SAMPLE_RATE as usize * 2;
    let mut iq = Vec::with_capacity(want);
    let mut last = 0usize;
    while iq.len() < want {
        let b = sdr.read_samples(256 * 1024)?;
        iq.extend_from_slice(&b);
        let secs = iq.len() / (SAMPLE_RATE as usize * 2);
        if secs != last {
            last = secs;
            eprint!("\r   {secs}/{seconds} s capturés ({} Mo)   ", iq.len() / 1_000_000);
        }
    }
    eprintln!();

    std::fs::create_dir_all("meteo")?;
    let out = format!("meteo/meteor_{}", chrono::Local::now().format("%Y%m%d_%H%M"));
    println!("   décodage LRPT via SatDump…");
    satdump::decode("meteor_m2-x_lrpt", &iq, SAMPLE_RATE, 0.0, std::path::Path::new(&out))?;
    println!("✅ Décodage terminé → {out}/  (images SatDump)");
    Ok(())
}

/// Recherche les stations FM les plus fortes ; renvoie la fréquence du pic (Hz).
fn scan_fm(sdr: &RtlSdr, tuner: &mut R820t2) -> Result<u32> {
    println!("🔎 Recherche de stations FM (87,5–108 MHz)…");
    let mut results: Vec<(u32, f64)> = Vec::new();
    let mut f = 87_500_000u32;
    while f <= 108_000_000 {
        sdr.set_i2c_repeater(true)?;
        tuner.set_freq(sdr, f)?;
        sdr.set_i2c_repeater(false)?;
        sdr.reset_buffer()?;
        let _ = sdr.read_samples(16 * 1024)?; // jette le buffer périmé
        let b = sdr.read_samples(16 * 1024)?;
        results.push((f, fm::rms(&b)));
        f += 100_000;
    }
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    println!("   Stations les plus fortes :");
    for (freq, r) in results.iter().take(6) {
        println!("     {:>6.1} MHz   RMS {r:5.1}", *freq as f64 / 1e6);
    }
    Ok(results[0].0)
}

/// Capture la FM et écrit un WAV audio.
fn run_fm(sdr: &RtlSdr, tuner: &mut R820t2, freq: Option<f64>, hp_hz: f32) -> Result<()> {
    let center = match freq {
        Some(mhz) => (mhz * 1e6) as u32,
        None => scan_fm(sdr, tuner)?,
    };
    // Accord grossier puis auto-centrage sur la porteuse exacte.
    sdr.set_i2c_repeater(true)?;
    tuner.set_freq(sdr, center)?;
    sdr.set_i2c_repeater(false)?;
    sdr.reset_buffer()?;
    let _ = sdr.read_samples(256 * 1024)?; // jette le buffer périmé
    let probe = sdr.read_samples(512 * 1024)?;
    let offset = fm::estimate_offset_hz(&probe);
    let tuned = (center as i32 + offset).max(0) as u32;
    println!(
        "\n🎚  Station {:.1} MHz, décalage mesuré {:+} kHz → accord {:.3} MHz",
        center as f64 / 1e6,
        offset / 1000,
        tuned as f64 / 1e6
    );

    sdr.set_i2c_repeater(true)?;
    tuner.set_freq(sdr, tuned)?;
    sdr.set_i2c_repeater(false)?;
    sdr.reset_buffer()?;
    let _ = sdr.read_samples(256 * 1024)?; // jette le buffer périmé

    let want = 6 * SAMPLE_RATE as usize * 2; // ~6 s d'IQ
    let mut iq = Vec::with_capacity(want);
    while iq.len() < want {
        let b = sdr.read_samples(256 * 1024)?;
        iq.extend_from_slice(&b);
    }
    let sat = iq.iter().filter(|&&b| b == 0 || b == 255).count() as f64 / iq.len() as f64;
    println!(
        "   {} Mo capturés (saturation ADC {:.2} %), démodulation WBFM…",
        iq.len() / 1_000_000,
        sat * 100.0
    );

    println!("   passe-haut anti-rumble : {hp_hz:.0} Hz");
    let audio = fm::demod_wbfm(&iq, hp_hz);
    let path = "fm.wav";
    fm::write_wav(path, &audio, fm::AUDIO_RATE)?;
    println!(
        "✅ Écrit {path} ({:.1} s @ {} Hz mono).\n   Joue-le :  start {path}",
        audio.len() as f64 / fm::AUDIO_RATE as f64,
        fm::AUDIO_RATE
    );
    Ok(())
}

/// Balayage de puissance (vérification du tuner).
fn power_scan(sdr: &RtlSdr, tuner: &mut R820t2) -> Result<()> {
    println!("Balayage de puissance (le tuner fonctionne si la bande FM ressort) :\n");
    println!("   Fréq (MHz)    RMS    dBFS   sat%   barre");
    let scan = [
        88.0, 90.0, 92.0, 94.0, 96.0, 98.0, 100.0, 102.0, 104.0, 106.0, 108.0, // bande FM
        300.0, 600.0, 1090.0, // références hors FM
    ];
    for f in scan {
        let (rms, dbfs, sat) = measure_power(sdr, tuner, (f * 1e6) as u32)?;
        let bar = "#".repeat(((rms / 4.0) as usize).min(40));
        println!("   {f:8.1}   {rms:6.2}  {dbfs:6.1}  {:5.1}   {bar}", sat * 100.0);
    }
    Ok(())
}

/// Récepteur ADS-B live sur 1090 MHz.
fn run_adsb(sdr: &RtlSdr, tuner: &mut R820t2) -> Result<()> {
    // Auto-test du décodeur avant de dépendre du ciel.
    adsb::self_test();
    println!();

    sdr.set_i2c_repeater(true)?;
    tuner.set_freq(sdr, ADSB_FREQ)?;
    sdr.set_i2c_repeater(false)?;
    sdr.reset_buffer()?;
    let _ = sdr.read_samples(256 * 1024)?; // jette le buffer périmé

    println!("📡 Écoute ADS-B sur 1090 MHz… (Ctrl+C pour arrêter)\n");

    let mut demod = adsb::Demod::new();
    let mut buffers = 0u64;
    loop {
        let buf = sdr.read_samples(256 * 1024)?;
        demod.process(&buf);
        buffers += 1;
        if buffers % 16 == 0 {
            let (planes, msgs, pre) = demod.stats();
            eprintln!("   … {planes} avion(s), {msgs} message(s) valides, {pre} préambule(s) détecté(s)");
        }
    }
}
