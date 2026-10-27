// SPDX-License-Identifier: LGPL-3.0-only
//! ICC Color Profile representation and parser (`QIccProfile` equivalent).
//!
//! Parses the standard 128-byte ICC profile header (ICC.1:2010 specification),
//! extracting magic signature ('acsp'), profile class, color space signature,
//! and connection space.

use super::color_space::{ColorSpace, PrimaryPoints, Primaries, TransferFunction};
use crate::geometry::PointF;
use super::pixel_format::ColorModel;

/// Parsed ICC Profile header metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IccHeader {
    pub profile_size: u32,
    pub cmm_type: [u8; 4],
    pub version_major: u8,
    pub version_minor: u8,
    pub device_class: [u8; 4],
    pub color_space: [u8; 4],
    pub pcs: [u8; 4],
    pub platform: [u8; 4],
}

/// Standard ICC Profile (`QIccProfile`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IccProfile {
    data: Vec<u8>,
    header: Option<IccHeader>,
}

impl Default for IccProfile {
    fn default() -> Self {
        Self::new()
    }
}

impl IccProfile {
    /// Magic signature 'acsp' at offset 36..40.
    pub const MAGIC: [u8; 4] = *b"acsp";

    /// Creates an empty invalid ICC profile.
    pub const fn new() -> Self {
        Self {
            data: Vec::new(),
            header: None,
        }
    }

    /// Parses an ICC profile from raw binary bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 128 {
            return None;
        }

        // Validate 'acsp' magic
        if data[36..40] != Self::MAGIC {
            return None;
        }

        let profile_size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let cmm_type = [data[4], data[5], data[6], data[7]];
        let version_major = data[8];
        let version_minor = data[9];
        let device_class = [data[12], data[13], data[14], data[15]];
        let color_space = [data[16], data[17], data[18], data[19]];
        let pcs = [data[20], data[21], data[22], data[23]];
        let platform = [data[40], data[41], data[42], data[43]];

        let header = IccHeader {
            profile_size,
            cmm_type,
            version_major,
            version_minor,
            device_class,
            color_space,
            pcs,
            platform,
        };

        Some(Self {
            data: data.to_vec(),
            header: Some(header),
        })
    }

    /// Whether this profile is parsed and valid.
    pub fn is_valid(&self) -> bool {
        self.header.is_some()
    }

    /// Parsed header metadata, if valid.
    pub fn header(&self) -> Option<&IccHeader> {
        self.header.as_ref()
    }

    /// Raw profile binary data.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Extracted color model signature.
    pub fn color_model(&self) -> Option<ColorModel> {
        let hdr = self.header.as_ref()?;
        match &hdr.color_space {
            b"RGB " => Some(ColorModel::Rgb),
            b"GRAY" => Some(ColorModel::Grayscale),
            b"CMYK" => Some(ColorModel::Cmyk),
            _ => None,
        }
    }

    /// Looks up a tag in the tag table; returns its data slice.
    fn tag(&self, sig: &[u8; 4]) -> Option<&[u8]> {
        let d = &self.data;
        let count = u32::from_be_bytes(d.get(128..132)?.try_into().ok()?) as usize;
        for i in 0..count.min(1024) {
            let e = 132 + i * 12;
            let ent = d.get(e..e + 12)?;
            if &ent[0..4] == sig {
                let off = u32::from_be_bytes(ent[4..8].try_into().ok()?) as usize;
                let size = u32::from_be_bytes(ent[8..12].try_into().ok()?) as usize;
                return d.get(off..off.checked_add(size)?);
            }
        }
        None
    }

    /// Parses an `XYZ ` tag into CIE XYZ.
    fn xyz_tag(&self, sig: &[u8; 4]) -> Option<[f32; 3]> {
        let t = self.tag(sig)?;
        if t.len() < 20 || &t[0..4] != b"XYZ " {
            return None;
        }
        let f = |o: usize| {
            i32::from_be_bytes([t[o], t[o + 1], t[o + 2], t[o + 3]]) as f32 / 65536.0
        };
        Some([f(8), f(12), f(16)])
    }

    /// Parses a `curv` / `para` tone reproduction curve tag into a transfer function.
    fn trc_tag(&self, sig: &[u8; 4]) -> Option<TransferFunction> {
        let t = self.tag(sig)?;
        let kind = t.get(0..4)?;
        let s15 = |o: usize| -> Option<f32> {
            Some(i32::from_be_bytes(t.get(o..o + 4)?.try_into().ok()?) as f32 / 65536.0)
        };
        let eval: Box<dyn Fn(f32) -> f32> = if kind == b"curv" {
            let n = u32::from_be_bytes(t.get(8..12)?.try_into().ok()?) as usize;
            match n {
                0 => return Some(TransferFunction::Linear),
                1 => {
                    let g = u16::from_be_bytes(t.get(12..14)?.try_into().ok()?) as f32 / 256.0;
                    return Some(Self::classify_gamma(g));
                }
                _ => {
                    let body = t.get(12..12usize.checked_add(n.checked_mul(2)?)?)?;
                    let table: Vec<f32> = body
                        .chunks_exact(2)
                        .map(|c| u16::from_be_bytes([c[0], c[1]]) as f32 / 65535.0)
                        .collect();
                    Box::new(move |x| {
                        let p = x.clamp(0.0, 1.0) * (table.len() - 1) as f32;
                        let i = (p.floor() as usize).min(table.len() - 1);
                        let j = (i + 1).min(table.len() - 1);
                        table[i] + (table[j] - table[i]) * (p - i as f32)
                    })
                }
            }
        } else if kind == b"para" {
            let ty = u16::from_be_bytes(t.get(8..10)?.try_into().ok()?);
            let g = s15(12)?;
            if ty == 0 {
                return Some(Self::classify_gamma(g));
            }
            // Parameter order after `g`: a b c d e f (ICC.1 parametricCurveType).
            let a = s15(16)?;
            let b = s15(20)?;
            let knee = -b / a;
            let (c, d, e, f) = match ty {
                1 => (0.0, knee, 0.0, 0.0),
                2 => (0.0, knee, s15(24)?, s15(24)?),
                3 => (s15(24)?, s15(28)?, 0.0, 0.0),
                4 => (s15(24)?, s15(28)?, s15(32)?, s15(36)?),
                _ => return None,
            };
            Box::new(move |x| {
                if x >= d {
                    (a * x + b).max(0.0).powf(g) + e
                } else {
                    c * x + f
                }
            })
        } else {
            return None;
        };
        Some(Self::classify_curve(&*eval))
    }

    fn classify_gamma(g: f32) -> TransferFunction {
        if (g - 1.0).abs() < 1e-3 {
            TransferFunction::Linear
        } else {
            TransferFunction::Gamma(g)
        }
    }

    /// Maps a sampled curve to linear, sRGB, or a fitted pure gamma.
    fn classify_curve(eval: &dyn Fn(f32) -> f32) -> TransferFunction {
        let srgb = ColorSpace::srgb();
        let samples = 17;
        let close = |f: &dyn Fn(f32) -> f32, tol: f32| {
            (0..=samples).all(|i| {
                let x = i as f32 / samples as f32;
                (eval(x) - f(x)).abs() <= tol
            })
        };
        if close(&|x| x, 0.003) {
            return TransferFunction::Linear;
        }
        if close(&|x| srgb.to_linear(x), 0.005) {
            return TransferFunction::Srgb;
        }
        // Pure gamma fitted at the mid-tone sample.
        let y = eval(0.5).clamp(1e-4, 0.9999);
        Self::classify_gamma(y.ln() / 0.5f32.ln())
    }

    /// Bradford-adapts a D50 XYZ colour to D65 and returns its xy chromaticity.
    fn d50_xyz_to_d65_xy(xyz: [f32; 3]) -> PointF {
        const BFD: [[f32; 3]; 3] = [
            [0.8951, 0.2664, -0.1614],
            [-0.7502, 1.7135, 0.0367],
            [0.0389, -0.0685, 1.0296],
        ];
        let mul = |m: &[[f32; 3]; 3], v: [f32; 3]| {
            [
                m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
                m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
                m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
            ]
        };
        let wp = |p: PointF| [p.x / p.y, 1.0, (1.0 - p.x - p.y) / p.y];
        let s = mul(&BFD, wp(PrimaryPoints::D50));
        let d = mul(&BFD, wp(PrimaryPoints::D65));
        let c = mul(&BFD, xyz);
        let c = [c[0] * d[0] / s[0], c[1] * d[1] / s[1], c[2] * d[2] / s[2]];
        // Inverse Bradford.
        let inv = [
            [0.9869929, -0.1470543, 0.1599627],
            [0.4323053, 0.5183603, 0.0492912],
            [-0.0085287, 0.0400428, 0.9684867],
        ];
        let o = mul(&inv, c);
        let sum = o[0] + o[1] + o[2];
        PointF {
            x: o[0] / sum,
            y: o[1] / sum,
        }
    }

    /// Attempts to convert this profile to a `ColorSpace`.
    ///
    /// RGB profiles use the `rXYZ`/`gXYZ`/`bXYZ`/`wtpt` colorants and `rTRC` curve;
    /// Gray profiles use `kTRC`. Well-known gamuts/curves map to the named spaces.
    /// Profiles lacking the required tags fall back to sRGB (RGB) / linear gray.
    pub fn to_color_space(&self) -> Option<ColorSpace> {
        match self.color_model()? {
            ColorModel::Rgb => Some(self.rgb_color_space().unwrap_or_else(ColorSpace::srgb)),
            ColorModel::Grayscale => Some(match self.trc_tag(b"kTRC") {
                Some(tf) => ColorSpace::from_primary_points(
                    Primaries::Srgb,
                    PrimaryPoints::from_primaries(Primaries::Srgb),
                    tf,
                    "ICC Gray",
                ),
                None => ColorSpace::linear_srgb(),
            }),
            _ => None,
        }
    }

    fn rgb_color_space(&self) -> Option<ColorSpace> {
        let r = self.xyz_tag(b"rXYZ")?;
        let g = self.xyz_tag(b"gXYZ")?;
        let b = self.xyz_tag(b"bXYZ")?;
        let tf = self.trc_tag(b"rTRC")?;
        let xy = |v: [f32; 3]| {
            let s = v[0] + v[1] + v[2];
            PointF {
                x: v[0] / s,
                y: v[1] / s,
            }
        };
        let white = self.xyz_tag(b"wtpt").map(xy).unwrap_or(PrimaryPoints::D50);

        // Recognise standard gamuts by comparing D65-adapted chromaticities.
        let adapted = [
            Self::d50_xyz_to_d65_xy(r),
            Self::d50_xyz_to_d65_xy(g),
            Self::d50_xyz_to_d65_xy(b),
        ];
        let matches = |p: Primaries| {
            let q = PrimaryPoints::from_primaries(p);
            let pts = [q.red_point, q.green_point, q.blue_point];
            adapted
                .iter()
                .zip(pts)
                .all(|(a, q)| (a.x - q.x).abs() < 0.004 && (a.y - q.y).abs() < 0.004)
        };
        let gamma_near = |t: f32| matches!(tf, TransferFunction::Gamma(g) if (g - t).abs() < 0.05);
        if matches(Primaries::Srgb) {
            match tf {
                TransferFunction::Srgb => return Some(ColorSpace::srgb()),
                TransferFunction::Linear => return Some(ColorSpace::linear_srgb()),
                _ => {}
            }
        } else if matches(Primaries::DisplayP3) && tf == TransferFunction::Srgb {
            return Some(ColorSpace::display_p3());
        } else if matches(Primaries::AdobeRgb) && gamma_near(2.2) {
            return Some(ColorSpace::adobe_rgb());
        }

        // Custom colorants: keep the profile's own (D50 PCS-adapted) matrix exactly.
        let named = [
            Primaries::Srgb,
            Primaries::DisplayP3,
            Primaries::AdobeRgb,
            Primaries::ProPhotoRgb,
            Primaries::Bt2020,
        ]
        .into_iter()
        .find(|p| matches(*p))
        .unwrap_or(Primaries::Custom);
        Some(ColorSpace::from_primary_points(
            named,
            PrimaryPoints {
                white_point: white,
                red_point: xy(r),
                green_point: xy(g),
                blue_point: xy(b),
            },
            tf,
            "ICC RGB",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s15(v: f32) -> [u8; 4] {
        ((v * 65536.0).round() as i32).to_be_bytes()
    }

    fn xyz_tag(v: [f32; 3]) -> Vec<u8> {
        let mut t = b"XYZ \0\0\0\0".to_vec();
        for c in v {
            t.extend_from_slice(&s15(c));
        }
        t
    }

    fn gamma_tag(g: f32) -> Vec<u8> {
        let mut t = b"curv\0\0\0\0".to_vec();
        t.extend_from_slice(&1u32.to_be_bytes());
        t.extend_from_slice(&((g * 256.0).round() as u16).to_be_bytes());
        t.extend_from_slice(&[0, 0]);
        t
    }

    fn srgb_para_tag() -> Vec<u8> {
        let mut t = b"para\0\0\0\0".to_vec();
        t.extend_from_slice(&[0, 3, 0, 0]);
        for v in [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
            t.extend_from_slice(&s15(v));
        }
        t
    }

    fn build(space: &[u8; 4], tags: &[([u8; 4], Vec<u8>)]) -> IccProfile {
        let mut data = vec![0u8; 128];
        data[16..20].copy_from_slice(space);
        data[36..40].copy_from_slice(b"acsp");
        let mut body = Vec::new();
        let base = 128 + 4 + tags.len() * 12;
        data.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        for (sig, t) in tags {
            data.extend_from_slice(sig);
            data.extend_from_slice(&((base + body.len()) as u32).to_be_bytes());
            data.extend_from_slice(&(t.len() as u32).to_be_bytes());
            body.extend_from_slice(t);
            while body.len() % 4 != 0 {
                body.push(0);
            }
        }
        data.extend_from_slice(&body);
        IccProfile::from_bytes(&data).unwrap()
    }

    /// D50-adapted colorants for a space (Bradford), as stored in ICC profiles.
    fn rgb_profile(cs: &ColorSpace, trc: Vec<u8>, wp: PointF) -> IccProfile {
        // Build the D50 PCS matrix by adapting the space's own D65 matrix.
        let m = cs.rgb_to_xyz_matrix();
        let bfd = [
            [0.8951f32, 0.2664, -0.1614],
            [-0.7502, 1.7135, 0.0367],
            [0.0389, -0.0685, 1.0296],
        ];
        let inv = [
            [0.9869929f32, -0.1470543, 0.1599627],
            [0.4323053, 0.5183603, 0.0492912],
            [-0.0085287, 0.0400428, 0.9684867],
        ];
        let w = |p: PointF| [p.x / p.y, 1.0, (1.0 - p.x - p.y) / p.y];
        let mulv = |m: &[[f32; 3]; 3], v: [f32; 3]| {
            [
                m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
                m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
                m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
            ]
        };
        let s = mulv(&bfd, w(cs.primary_points().white_point));
        let d = mulv(&bfd, w(PrimaryPoints::D50));
        let col = |c: usize| {
            let v = mulv(&bfd, [m[0][c], m[1][c], m[2][c]]);
            mulv(&inv, [v[0] * d[0] / s[0], v[1] * d[1] / s[1], v[2] * d[2] / s[2]])
        };
        build(
            b"RGB ",
            &[
                (*b"rXYZ", xyz_tag(col(0))),
                (*b"gXYZ", xyz_tag(col(1))),
                (*b"bXYZ", xyz_tag(col(2))),
                (*b"wtpt", xyz_tag(w(wp))),
                (*b"rTRC", trc.clone()),
                (*b"gTRC", trc.clone()),
                (*b"bTRC", trc),
            ],
        )
    }

    #[test]
    fn icc_rgb_profiles_map_to_distinct_color_spaces() {
        let d50 = PrimaryPoints::D50;
        let srgb = rgb_profile(&ColorSpace::srgb(), srgb_para_tag(), d50)
            .to_color_space()
            .unwrap();
        let p3 = rgb_profile(&ColorSpace::display_p3(), srgb_para_tag(), d50)
            .to_color_space()
            .unwrap();
        let adobe = rgb_profile(&ColorSpace::adobe_rgb(), gamma_tag(2.19921875), d50)
            .to_color_space()
            .unwrap();
        assert_eq!(srgb, ColorSpace::srgb());
        assert_eq!(p3, ColorSpace::display_p3());
        assert_eq!(adobe, ColorSpace::adobe_rgb());

        // P3 / Adobe green lie outside sRGB (negative red/blue, clamped to 0 on output),
        // while sRGB -> sRGB is the identity: the transforms must differ.
        let (r, _, b) = p3.transformation_to_color_space(&srgb).map_rgb(0.0, 1.0, 0.0);
        assert!(r == 0.0 && b == 0.0);
        let (r, _, _) = adobe.transformation_to_color_space(&srgb).map_rgb(0.0, 1.0, 0.0);
        assert!(r == 0.0);
        assert!(srgb.transformation_to_color_space(&ColorSpace::srgb()).is_identity());
        assert_ne!(p3, adobe);
    }

    #[test]
    fn icc_custom_gamma_and_gray_ktrc() {
        let mut custom = ColorSpace::srgb();
        custom = ColorSpace::from_primary_points(
            Primaries::Custom,
            PrimaryPoints {
                white_point: PrimaryPoints::D65,
                red_point: PointF { x: 0.70, y: 0.28 },
                green_point: PointF { x: 0.20, y: 0.74 },
                blue_point: PointF { x: 0.14, y: 0.05 },
            },
            TransferFunction::Gamma(1.8),
            custom.description().to_string(),
        );
        let cs = rgb_profile(&custom, gamma_tag(1.8), PrimaryPoints::D50)
            .to_color_space()
            .unwrap();
        assert_eq!(cs.primaries(), Primaries::Custom);
        match cs.transfer_function() {
            TransferFunction::Gamma(g) => assert!((g - 1.8).abs() < 0.01),
            other => panic!("{other:?}"),
        }
        let to_srgb = cs.transformation_to_color_space(&ColorSpace::srgb());
        let (r, g, b) = to_srgb.map_rgb(1.0, 0.0, 0.0);
        assert!(r > 1.0 && g < 0.01 && b < 0.2, "got {r} {g} {b}");

        let gray = build(b"GRAY", &[(*b"kTRC", gamma_tag(2.2))]).to_color_space().unwrap();
        assert!(
            matches!(gray.transfer_function(), TransferFunction::Gamma(g) if (g - 2.2).abs() < 0.01)
        );
        let lin = build(b"GRAY", &[(*b"kTRC", gamma_tag(1.0))]).to_color_space().unwrap();
        assert!(lin.is_linear());
    }
}
