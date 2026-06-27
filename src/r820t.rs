//! Driver du tuner Rafael Micro R820T/R820T2, porté fidèlement de
//! `tuner_r82xx.c` (osmocom/librtlsdr, GPLv2+).
//!
//! Le tuner est piloté en I2C derrière le RTL2832U. Toutes les méthodes
//! prennent un `&RtlSdr` pour les accès I2C ; l'appelant doit avoir activé
//! le « repeater » I2C (`set_i2c_repeater(true)`) avant de les invoquer.

use crate::{Result, RtlSdr};

pub const R820T_I2C_ADDR: u8 = 0x34;
const XTAL: u32 = 28_800_000;
const REG_SHADOW_START: usize = 5;
const NUM_REGS: usize = 30;
const VER_NUM: u8 = 49;
/// IF utilisée par le R82xx en mode 6 MHz (le RTL2832U la redescend en bande de base).
pub const IF_FREQ: u32 = 3_570_000;

/// Valeurs initiales des registres 0x05..0x1f (27), complétées de 3 zéros
/// (0x20..0x22) pour atteindre NUM_REGS, comme dans librtlsdr.
const INIT_ARRAY: [u8; NUM_REGS] = [
    0x83, 0x32, 0x75, // 05..07
    0xc0, 0x40, 0xd6, 0x6c, // 08..0b
    0xf5, 0x63, 0x75, 0x68, // 0c..0f
    0x6c, 0x83, 0x80, 0x00, // 10..13
    0x0f, 0x00, 0xc0, 0x30, // 14..17
    0x48, 0xcc, 0x60, 0x00, // 18..1b
    0x54, 0xae, 0x4a, 0xc0, // 1c..1f
    0x00, 0x00, 0x00, // 20..22 (zéro-init en C)
];

struct FreqRange {
    freq: u32, // MHz, borne basse
    open_d: u8,
    rf_mux_ploy: u8,
    tf_c: u8,
}

const FREQ_RANGES: &[FreqRange] = &[
    FreqRange { freq: 0, open_d: 0x08, rf_mux_ploy: 0x02, tf_c: 0xdf },
    FreqRange { freq: 50, open_d: 0x08, rf_mux_ploy: 0x02, tf_c: 0xbe },
    FreqRange { freq: 55, open_d: 0x08, rf_mux_ploy: 0x02, tf_c: 0x8b },
    FreqRange { freq: 60, open_d: 0x08, rf_mux_ploy: 0x02, tf_c: 0x7b },
    FreqRange { freq: 65, open_d: 0x08, rf_mux_ploy: 0x02, tf_c: 0x69 },
    FreqRange { freq: 70, open_d: 0x08, rf_mux_ploy: 0x02, tf_c: 0x58 },
    FreqRange { freq: 75, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x44 },
    FreqRange { freq: 80, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x44 },
    FreqRange { freq: 90, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x34 },
    FreqRange { freq: 100, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x34 },
    FreqRange { freq: 110, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x24 },
    FreqRange { freq: 120, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x24 },
    FreqRange { freq: 140, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x14 },
    FreqRange { freq: 180, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x13 },
    FreqRange { freq: 220, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x13 },
    FreqRange { freq: 250, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x11 },
    FreqRange { freq: 280, open_d: 0x00, rf_mux_ploy: 0x02, tf_c: 0x00 },
    FreqRange { freq: 310, open_d: 0x00, rf_mux_ploy: 0x41, tf_c: 0x00 },
    FreqRange { freq: 450, open_d: 0x00, rf_mux_ploy: 0x41, tf_c: 0x00 },
    FreqRange { freq: 588, open_d: 0x00, rf_mux_ploy: 0x40, tf_c: 0x00 },
    FreqRange { freq: 650, open_d: 0x00, rf_mux_ploy: 0x40, tf_c: 0x00 },
];

// Pas de gain (en dixièmes de dB) mesurés par Steve Markgraf.
const LNA_GAIN_STEPS: [i32; 16] = [0, 9, 13, 40, 38, 13, 31, 22, 26, 31, 26, 14, 19, 5, 35, 13];
const MIXER_GAIN_STEPS: [i32; 16] = [0, 5, 10, 10, 19, 9, 10, 25, 17, 10, 8, 16, 13, 6, 3, -8];

fn bitrev(byte: u8) -> u8 {
    const LUT: [u8; 16] = [
        0x0, 0x8, 0x4, 0xc, 0x2, 0xa, 0x6, 0xe, 0x1, 0x9, 0x5, 0xd, 0x3, 0xb, 0x7, 0xf,
    ];
    (LUT[(byte & 0xf) as usize] << 4) | LUT[(byte >> 4) as usize]
}

fn mask8(reg: u8, val: u8, mask: u8) -> u8 {
    (reg & !mask) | (val & mask)
}

/// État du tuner : registres « shadow » (écriture seule côté matériel) + contexte.
pub struct R820t2 {
    regs: [u8; NUM_REGS],
    int_freq: u32,
    fil_cal_code: u8,
    has_lock: bool,
}

impl R820t2 {
    pub fn new() -> Self {
        Self { regs: [0; NUM_REGS], int_freq: IF_FREQ, fil_cal_code: 0, has_lock: false }
    }

    // --- Accès registres (avec shadow) ---

    fn write(&mut self, rtl: &RtlSdr, reg: u8, vals: &[u8]) -> Result<()> {
        rtl.i2c_write(R820T_I2C_ADDR, reg, vals)?;
        for (i, v) in vals.iter().enumerate() {
            let idx = reg as usize + i;
            if (REG_SHADOW_START..REG_SHADOW_START + NUM_REGS).contains(&idx) {
                self.regs[idx - REG_SHADOW_START] = *v;
            }
        }
        Ok(())
    }

    fn write_reg(&mut self, rtl: &RtlSdr, reg: u8, val: u8) -> Result<()> {
        self.write(rtl, reg, &[val])
    }

    fn read_cache(&self, reg: u8) -> u8 {
        self.regs[reg as usize - REG_SHADOW_START]
    }

    fn write_reg_mask(&mut self, rtl: &RtlSdr, reg: u8, val: u8, mask: u8) -> Result<()> {
        let v = mask8(self.read_cache(reg), val, mask);
        self.write(rtl, reg, &[v])
    }

    /// Lecture des registres d'état (à partir de 0x00), bits inversés.
    fn read(&self, rtl: &RtlSdr, buf: &mut [u8]) -> Result<()> {
        rtl.i2c_read(R820T_I2C_ADDR, 0x00, buf)?;
        for b in buf.iter_mut() {
            *b = bitrev(*b);
        }
        Ok(())
    }

    // --- Logique d'accord ---

    fn set_mux(&mut self, rtl: &RtlSdr, freq_hz: u32) -> Result<()> {
        let freq_mhz = freq_hz / 1_000_000;
        let mut i = 0;
        while i < FREQ_RANGES.len() - 1 {
            if freq_mhz < FREQ_RANGES[i + 1].freq {
                break;
            }
            i += 1;
        }
        let range = &FREQ_RANGES[i];

        self.write_reg_mask(rtl, 0x17, range.open_d, 0x08)?;
        self.write_reg_mask(rtl, 0x1a, range.rf_mux_ploy, 0xc3)?;
        self.write_reg(rtl, 0x1b, range.tf_c)?;
        // xtal_cap_sel = HIGH_CAP_0P → 0x00
        self.write_reg_mask(rtl, 0x10, 0x00, 0x0b)?;
        self.write_reg_mask(rtl, 0x08, 0x00, 0x3f)?;
        self.write_reg_mask(rtl, 0x09, 0x00, 0x3f)?;
        Ok(())
    }

    fn set_pll(&mut self, rtl: &RtlSdr, freq: u32) -> Result<()> {
        let vco_min: u32 = 1_770_000; // kHz
        let vco_max = vco_min * 2;
        let freq_khz = (freq + 500) / 1000;
        let pll_ref = XTAL;

        // pll autotune = 128 kHz
        self.write_reg_mask(rtl, 0x1a, 0x00, 0x0c)?;

        // snapshot des registres 0x10..0x16
        let base = 0x10 - REG_SHADOW_START;
        let mut regs = [0u8; 7];
        regs.copy_from_slice(&self.regs[base..base + 7]);

        regs[0] = mask8(regs[0], 0x00, 0x10); // refdiv2 = 0
        regs[2] = mask8(regs[2], 0x80, 0xe0); // VCO current = 100

        // diviseur
        let mut mix_div: u32 = 2;
        let mut div_num: i32 = 0;
        while mix_div <= 64 {
            if freq_khz * mix_div >= vco_min && freq_khz * mix_div < vco_max {
                let mut div_buf = mix_div;
                while div_buf > 2 {
                    div_buf >>= 1;
                    div_num += 1;
                }
                break;
            }
            mix_div <<= 1;
        }

        let mut data = [0u8; 5];
        self.read(rtl, &mut data)?;
        let vco_power_ref: u32 = 2; // R820T
        let vco_fine_tune = u32::from((data[4] & 0x30) >> 4);
        if vco_fine_tune > vco_power_ref {
            div_num -= 1;
        } else if vco_fine_tune < vco_power_ref {
            div_num += 1;
        }
        regs[0] = mask8(regs[0], (div_num as u8) << 5, 0xe0);

        let vco_freq = u64::from(freq) * u64::from(mix_div);
        let vco_div = (u64::from(pll_ref) + 65536 * vco_freq) / (2 * u64::from(pll_ref));
        let nint = (vco_div / 65536) as u32;
        let sdm = (vco_div % 65536) as u32;

        if nint > (128 / vco_power_ref) - 1 {
            return Err(format!("R82xx : pas de valeurs PLL valides pour {freq} Hz").into());
        }

        let ni = (nint - 13) / 4;
        let si = nint - 4 * ni - 13;
        regs[4] = (ni + (si << 6)) as u8;
        regs[2] = mask8(regs[2], if sdm == 0 { 0x08 } else { 0x00 }, 0x08);
        regs[5] = (sdm & 0xff) as u8;
        regs[6] = (sdm >> 8) as u8;

        self.write(rtl, 0x10, &regs)?;

        // contrôle du verrouillage PLL (2 tentatives)
        self.has_lock = false;
        for i in 0..2 {
            let mut d = [0u8; 3];
            self.read(rtl, &mut d)?;
            if d[2] & 0x40 != 0 {
                self.has_lock = true;
                break;
            }
            if i == 0 {
                // augmente le courant VCO
                self.write_reg_mask(rtl, 0x12, 0x60, 0xe0)?;
            }
        }

        if self.has_lock {
            // pll autotune = 8 kHz
            self.write_reg_mask(rtl, 0x1a, 0x08, 0x08)?;
        }
        Ok(())
    }

    fn set_tv_standard(&mut self, rtl: &RtlSdr) -> Result<()> {
        let if_khz = 3570u32;
        let filt_cal_lo = 56000u32;
        let filt_gain = 0x10u8;
        let img_r = 0x00u8;
        let filt_q = 0x10u8;
        let hp_cor = 0x6bu8;
        let ext_enable = 0x60u8;
        let loop_through = 0x01u8;
        let lt_att = 0x00u8;
        let flt_ext_widest = 0x00u8;
        let polyfil_cur = 0x60u8;

        // réinitialise le shadow à partir du tableau d'init
        self.regs.copy_from_slice(&INIT_ARRAY);

        self.write_reg_mask(rtl, 0x0c, 0x00, 0x0f)?;
        self.write_reg_mask(rtl, 0x13, VER_NUM, 0x3f)?;
        // type != ANALOG_TV
        self.write_reg_mask(rtl, 0x1d, 0x00, 0x38)?;
        self.int_freq = if_khz * 1000;

        // calibration du filtre (forcée, une fois)
        for _ in 0..2 {
            self.write_reg_mask(rtl, 0x0b, hp_cor, 0x60)?;
            self.write_reg_mask(rtl, 0x0f, 0x04, 0x04)?;
            self.write_reg_mask(rtl, 0x10, 0x00, 0x03)?;
            self.set_pll(rtl, filt_cal_lo * 1000)?;
            if !self.has_lock {
                return Err("R82xx : PLL de calibration non verrouillée".into());
            }
            self.write_reg_mask(rtl, 0x0b, 0x10, 0x10)?; // start trigger
            self.write_reg_mask(rtl, 0x0b, 0x00, 0x10)?; // stop trigger
            self.write_reg_mask(rtl, 0x0f, 0x00, 0x04)?;
            let mut data = [0u8; 5];
            self.read(rtl, &mut data)?;
            self.fil_cal_code = data[4] & 0x0f;
            if self.fil_cal_code != 0 && self.fil_cal_code != 0x0f {
                break;
            }
        }
        if self.fil_cal_code == 0x0f {
            self.fil_cal_code = 0;
        }

        self.write_reg_mask(rtl, 0x0a, filt_q | self.fil_cal_code, 0x1f)?;
        self.write_reg_mask(rtl, 0x0b, hp_cor, 0xef)?;
        self.write_reg_mask(rtl, 0x07, img_r, 0x80)?;
        self.write_reg_mask(rtl, 0x06, filt_gain, 0x30)?;
        self.write_reg_mask(rtl, 0x1e, ext_enable, 0x60)?;
        self.write_reg_mask(rtl, 0x05, loop_through, 0x80)?;
        self.write_reg_mask(rtl, 0x1f, lt_att, 0x80)?;
        self.write_reg_mask(rtl, 0x0f, flt_ext_widest, 0x80)?;
        self.write_reg_mask(rtl, 0x19, polyfil_cur, 0x60)?;
        Ok(())
    }

    fn sysfreq_sel(&mut self, rtl: &RtlSdr) -> Result<()> {
        // delsys = DVB-T, freq = 0 → branche « else »
        let mixer_top = 0x24u8;
        let lna_top = 0xe5u8;
        let cp_cur = 0x38u8;
        let div_buf_cur = 0x30u8;
        let lna_vth_l = 0x53u8;
        let mixer_vth_l = 0x75u8;
        let air_cable1_in = 0x00u8;
        let cable2_in = 0x00u8;
        let lna_discharge = 14u8;
        let filter_cur = 0x40u8;

        // use_predetect = 0 → on saute l'écriture 0x06 pre_dect initiale
        self.write_reg_mask(rtl, 0x1d, lna_top, 0xc7)?;
        self.write_reg_mask(rtl, 0x1c, mixer_top, 0xf8)?;
        self.write_reg(rtl, 0x0d, lna_vth_l)?;
        self.write_reg(rtl, 0x0e, mixer_vth_l)?;
        self.write_reg_mask(rtl, 0x05, air_cable1_in, 0x60)?;
        self.write_reg_mask(rtl, 0x06, cable2_in, 0x08)?;
        self.write_reg_mask(rtl, 0x11, cp_cur, 0x38)?;
        self.write_reg_mask(rtl, 0x17, div_buf_cur, 0x30)?;
        self.write_reg_mask(rtl, 0x0a, filter_cur, 0x60)?;

        // réglage LNA (type != ANALOG_TV)
        self.write_reg_mask(rtl, 0x1d, 0x00, 0x38)?;
        self.write_reg_mask(rtl, 0x1c, 0x00, 0x04)?;
        self.write_reg_mask(rtl, 0x06, 0x00, 0x40)?;
        self.write_reg_mask(rtl, 0x1a, 0x30, 0x30)?;
        self.write_reg_mask(rtl, 0x1d, 0x18, 0x38)?;
        self.write_reg_mask(rtl, 0x1c, mixer_top, 0x04)?;
        self.write_reg_mask(rtl, 0x1e, lna_discharge, 0x1f)?;
        self.write_reg_mask(rtl, 0x1a, 0x20, 0x30)?;
        Ok(())
    }

    /// Initialisation complète du tuner (repeater I2C requis actif).
    pub fn init(&mut self, rtl: &RtlSdr) -> Result<()> {
        self.regs = [0; NUM_REGS];
        self.write(rtl, 0x05, &INIT_ARRAY)?;
        self.set_tv_standard(rtl)?;
        self.sysfreq_sel(rtl)?;
        Ok(())
    }

    /// Accorde le tuner sur la fréquence RF `freq` (Hz).
    pub fn set_freq(&mut self, rtl: &RtlSdr, freq: u32) -> Result<()> {
        let lo_freq = freq + self.int_freq;
        self.set_mux(rtl, lo_freq)?;
        self.set_pll(rtl, lo_freq)?;
        if !self.has_lock {
            return Err(format!("R82xx : PLL non verrouillée à {freq} Hz").into());
        }
        Ok(())
    }

    /// Gain manuel (LNA + mixer), `gain` en dixièmes de dB ; VGA fixe à 16,3 dB.
    pub fn set_gain_manual(&mut self, rtl: &RtlSdr, gain: i32) -> Result<()> {
        self.write_reg_mask(rtl, 0x05, 0x10, 0x10)?; // LNA auto off
        self.write_reg_mask(rtl, 0x07, 0x00, 0x10)?; // Mixer auto off
        let mut data = [0u8; 4];
        self.read(rtl, &mut data)?;
        self.write_reg_mask(rtl, 0x0c, 0x08, 0x9f)?; // VGA fixe 16,3 dB

        let mut total = 0i32;
        let mut lna_index = 0u8;
        let mut mix_index = 0u8;
        for _ in 0..15 {
            if total >= gain {
                break;
            }
            lna_index += 1;
            total += LNA_GAIN_STEPS[lna_index as usize];
            if total >= gain {
                break;
            }
            mix_index += 1;
            total += MIXER_GAIN_STEPS[mix_index as usize];
        }
        self.write_reg_mask(rtl, 0x05, lna_index, 0x0f)?;
        self.write_reg_mask(rtl, 0x07, mix_index, 0x0f)?;
        Ok(())
    }
}
