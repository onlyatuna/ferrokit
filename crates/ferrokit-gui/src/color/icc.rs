// SPDX-License-Identifier: LGPL-3.0-only
//! ICC Color Profile representation and parser (`QIccProfile` equivalent).
//!
//! Parses the standard 128-byte ICC profile header (ICC.1:2010 specification),
//! extracting magic signature ('acsp'), profile class, color space signature,
//! and connection space.

use super::color_space::{ColorSpace, NamedColorSpace, Primaries, PrimaryPoints, TransferFunction};
use super::pixel_format::ColorModel;
use crate::geometry::PointF;

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
        let f =
            |o: usize| i32::from_be_bytes([t[o], t[o + 1], t[o + 2], t[o + 3]]) as f32 / 65536.0;
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
                    return Self::classify_gamma(g);
                }
                _ => {
                    let body = t.get(12..12usize.checked_add(n.checked_mul(2)?)?)?;
                    let table: Vec<f32> = body
                        .as_chunks::<2>()
                        .0
                        .iter()
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
                return Self::classify_gamma(g);
            }
            if g <= 0.0 {
                return None; // a non-positive exponent is not a tone curve
            }
            // Parameter order after `g`: a b c d e f (ICC.1 parametricCurveType).
            let a = s15(16)?;
            if a == 0.0 {
                return None; // the knee `-b / a` and the curve itself are undefined
            }
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
        Self::classify_curve(&*eval)
    }

    fn classify_gamma(g: f32) -> Option<TransferFunction> {
        if !g.is_finite() || g <= 0.0 {
            return None;
        }
        Some(if (g - 1.0).abs() < 1e-3 {
            TransferFunction::Linear
        } else {
            TransferFunction::Gamma(g)
        })
    }

    /// Maps a sampled curve to linear, sRGB, or a fitted pure gamma; `None` when the curve is
    /// not finite over [0, 1] or cannot be fitted by a positive gamma.
    fn classify_curve(eval: &dyn Fn(f32) -> f32) -> Option<TransferFunction> {
        let srgb = ColorSpace::srgb();
        let samples = 17;
        if !(0..=samples).all(|i| eval(i as f32 / samples as f32).is_finite()) {
            return None;
        }
        let close = |f: &dyn Fn(f32) -> f32, tol: f32| {
            (0..=samples).all(|i| {
                let x = i as f32 / samples as f32;
                (eval(x) - f(x)).abs() <= tol
            })
        };
        if close(&|x| x, 0.003) {
            return Some(TransferFunction::Linear);
        }
        if close(&|x| srgb.to_linear(x), 0.005) {
            return Some(TransferFunction::Srgb);
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
    /// RGB profiles use the `rXYZ`/`gXYZ`/`bXYZ` colorants (D50 PCS-relative, as ICC requires)
    /// and the `rTRC`/`gTRC`/`bTRC` curves, one per channel; a missing or unparsable curve
    /// borrows the first parsable one (sRGB when none parses). Gray profiles use `kTRC`.
    /// Well-known gamuts/curves (sRGB, Display P3, Adobe RGB, ProPhoto RGB) map to the named
    /// spaces. Profiles lacking the required tags fall back to sRGB (RGB) / linear gray.
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

    /// Chromaticity of an XYZ colorant; `None` when it is not a usable colour (sum <= 0).
    fn xyz_to_xy(v: [f32; 3]) -> Option<PointF> {
        let s = v[0] + v[1] + v[2];
        (s.is_finite() && s > 0.0).then(|| PointF {
            x: v[0] / s,
            y: v[1] / s,
        })
    }

    fn rgb_color_space(&self) -> Option<ColorSpace> {
        let r = self.xyz_tag(b"rXYZ")?;
        let g = self.xyz_tag(b"gXYZ")?;
        let b = self.xyz_tag(b"bXYZ")?;
        // Each channel keeps its own curve. A channel whose curve is missing or unusable falls
        // back to the first parsable curve of the other channels, and when none parses the
        // gamut is still honoured with an sRGB curve.
        let parsed = [b"rTRC", b"gTRC", b"bTRC"].map(|sig| self.trc_tag(sig));
        let fallback = parsed
            .iter()
            .flatten()
            .next()
            .copied()
            .unwrap_or(TransferFunction::Srgb);
        let parsed_curves = parsed.map(|c| c.unwrap_or(fallback));
        // Fitted gammas that differ only by float noise (same curve sampled differently per
        // channel) are one curve, so a standard profile still maps to its named space.
        let same = |a: TransferFunction, b: TransferFunction| match (a, b) {
            (TransferFunction::Gamma(x), TransferFunction::Gamma(y)) => {
                (x - y).abs() <= 1e-4 * x.abs().max(y.abs())
            }
            _ => a == b,
        };
        let uniform =
            same(parsed_curves[0], parsed_curves[1]) && same(parsed_curves[0], parsed_curves[2]);
        let curves = if uniform {
            [parsed_curves[0]; 3]
        } else {
            parsed_curves
        };
        let tf = curves[0];

        // Profile colorants are PCS-relative, i.e. already adapted to D50.
        let d50 = [
            Self::xyz_to_xy(r)?,
            Self::xyz_to_xy(g)?,
            Self::xyz_to_xy(b)?,
        ];
        let d65 = [
            Self::d50_xyz_to_d65_xy(r),
            Self::d50_xyz_to_d65_xy(g),
            Self::d50_xyz_to_d65_xy(b),
        ];

        // A standard gamut is recognised by comparing the colorants in that gamut's own white
        // point: ProPhoto RGB is defined relative to D50, the others relative to D65.
        let matches = |p: Primaries| {
            let q = PrimaryPoints::from_primaries(p);
            let near_d50 = (q.white_point.x - PrimaryPoints::D50.x).abs() < 1e-3
                && (q.white_point.y - PrimaryPoints::D50.y).abs() < 1e-3;
            let have = if near_d50 { &d50 } else { &d65 };
            let pts = [q.red_point, q.green_point, q.blue_point];
            have.iter()
                .zip(pts)
                .all(|(a, q)| (a.x - q.x).abs() < 0.004 && (a.y - q.y).abs() < 0.004)
        };
        let gamma_near = |t: f32| matches!(tf, TransferFunction::Gamma(g) if (g - t).abs() < 0.05);
        if uniform {
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
            } else if matches(Primaries::ProPhotoRgb) && gamma_near(1.8) {
                return Some(ColorSpace::from_named(NamedColorSpace::ProPhotoRgb));
            }
        }

        // Custom curves or colorants: keep the profile's own (D50 PCS-adapted) matrix exactly.
        // Display P3 is labelled like `ColorSpace::display_p3()` (DCI-P3 primaries, D65).
        let named = [
            Primaries::Srgb,
            ColorSpace::display_p3().primaries(),
            Primaries::AdobeRgb,
            Primaries::ProPhotoRgb,
            Primaries::Bt2020,
        ]
        .into_iter()
        .find(|p| matches(*p))
        .unwrap_or(Primaries::Custom);
        Some(ColorSpace::from_primary_points_per_channel(
            named,
            PrimaryPoints {
                white_point: PrimaryPoints::D50,
                red_point: d50[0],
                green_point: d50[1],
                blue_point: d50[2],
            },
            curves,
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
    fn rgb_profile_curves(cs: &ColorSpace, trc: [Vec<u8>; 3], wp: PointF) -> IccProfile {
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
            mulv(
                &inv,
                [v[0] * d[0] / s[0], v[1] * d[1] / s[1], v[2] * d[2] / s[2]],
            )
        };
        build(
            b"RGB ",
            &[
                (*b"rXYZ", xyz_tag(col(0))),
                (*b"gXYZ", xyz_tag(col(1))),
                (*b"bXYZ", xyz_tag(col(2))),
                (*b"wtpt", xyz_tag(w(wp))),
                (*b"rTRC", trc[0].clone()),
                (*b"gTRC", trc[1].clone()),
                (*b"bTRC", trc[2].clone()),
            ],
        )
    }

    fn rgb_profile(cs: &ColorSpace, trc: Vec<u8>, wp: PointF) -> IccProfile {
        rgb_profile_curves(cs, [trc.clone(), trc.clone(), trc], wp)
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
        let adobe = rgb_profile(&ColorSpace::adobe_rgb(), gamma_tag(2.1992188), d50)
            .to_color_space()
            .unwrap();
        assert_eq!(srgb, ColorSpace::srgb());
        assert_eq!(p3, ColorSpace::display_p3());
        assert_eq!(adobe, ColorSpace::adobe_rgb());

        // P3 / Adobe green lie outside sRGB (negative red/blue, clamped to 0 on output),
        // while sRGB -> sRGB is the identity: the transforms must differ.
        let (r, _, b) = p3
            .transformation_to_color_space(&srgb)
            .map_rgb(0.0, 1.0, 0.0);
        assert!(r == 0.0 && b == 0.0);
        let (r, _, _) = adobe
            .transformation_to_color_space(&srgb)
            .map_rgb(0.0, 1.0, 0.0);
        assert!(r == 0.0);
        assert!(srgb
            .transformation_to_color_space(&ColorSpace::srgb())
            .is_identity());
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

        let gray = build(b"GRAY", &[(*b"kTRC", gamma_tag(2.2))])
            .to_color_space()
            .unwrap();
        assert!(
            matches!(gray.transfer_function(), TransferFunction::Gamma(g) if (g - 2.2).abs() < 0.01)
        );
        let lin = build(b"GRAY", &[(*b"kTRC", gamma_tag(1.0))])
            .to_color_space()
            .unwrap();
        assert!(lin.is_linear());
    }

    #[test]
    fn icc_unparsable_or_degenerate_curves_keep_the_gamut() {
        // Degenerate parametric curve (a == 0) is rejected.
        let mut bad = b"para\0\0\0\0".to_vec();
        bad.extend_from_slice(&[0, 3, 0, 0]);
        for v in [2.4, 0.0, 0.055, 1.0 / 12.92, 0.04045] {
            bad.extend_from_slice(&s15(v));
        }
        assert!(build(b"GRAY", &[(*b"kTRC", bad.clone())])
            .trc_tag(b"kTRC")
            .is_none());

        // rTRC unusable, gTRC valid: the valid curve is used.
        let p3 = ColorSpace::display_p3();
        let ok = rgb_profile(&p3, srgb_para_tag(), PrimaryPoints::D50);
        let mut tags = Vec::new();
        for sig in [b"rXYZ", b"gXYZ", b"bXYZ", b"wtpt"] {
            tags.push((*sig, ok.tag(sig).unwrap().to_vec()));
        }
        tags.push((*b"rTRC", bad.clone()));
        tags.push((*b"gTRC", srgb_para_tag()));
        assert_eq!(
            build(b"RGB ", &tags).to_color_space().unwrap(),
            ColorSpace::display_p3()
        );

        // No usable curve at all: still P3 primaries (sRGB-style curve), not plain sRGB.
        tags.pop();
        tags.pop();
        tags.push((*b"rTRC", bad));
        let cs = build(b"RGB ", &tags).to_color_space().unwrap();
        assert_eq!(cs, ColorSpace::display_p3());
        assert_ne!(cs, ColorSpace::srgb());
    }

    // ---- Data-driven gamut / curve parity tests -------------------------------------------

    fn curv_table(v: &[u16]) -> Vec<u8> {
        let mut t = b"curv\0\0\0\0".to_vec();
        t.extend_from_slice(&(v.len() as u32).to_be_bytes());
        for x in v {
            t.extend_from_slice(&x.to_be_bytes());
        }
        t
    }

    /// 256-entry `curv` table sampling `f` over [0, 1].
    fn sampled_curve(f: impl Fn(f32) -> f32) -> Vec<u8> {
        let v: Vec<u16> = (0..256)
            .map(|i| (f(i as f32 / 255.0) * 65535.0).round() as u16)
            .collect();
        curv_table(&v)
    }

    fn para_tag(ty: u16, params: &[f32]) -> Vec<u8> {
        let mut t = b"para\0\0\0\0".to_vec();
        t.extend_from_slice(&ty.to_be_bytes());
        t.extend_from_slice(&[0, 0]);
        for p in params {
            t.extend_from_slice(&s15(*p));
        }
        t
    }

    /// ROMM RGB / ProPhoto curve: gamma 1.8 with a linear toe below 1/32.
    fn romm_para_tag() -> Vec<u8> {
        para_tag(3, &[1.8, 1.0, 0.0, 1.0 / 16.0, 1.0 / 32.0])
    }

    fn prophoto() -> ColorSpace {
        ColorSpace::from_named(NamedColorSpace::ProPhotoRgb)
    }

    /// The colorant/white tags of `base` with the given TRC tags (`None` omits the tag).
    fn with_trcs(base: &IccProfile, curves: [Option<Vec<u8>>; 3]) -> IccProfile {
        let mut tags: Vec<([u8; 4], Vec<u8>)> = [b"rXYZ", b"gXYZ", b"bXYZ", b"wtpt"]
            .iter()
            .map(|s| (**s, base.tag(s).unwrap().to_vec()))
            .collect();
        for (sig, c) in [b"rTRC", b"gTRC", b"bTRC"].iter().zip(curves) {
            if let Some(c) = c {
                tags.push((**sig, c));
            }
        }
        build(b"RGB ", &tags)
    }

    struct Case {
        name: &'static str,
        gamut: ColorSpace,
        curve: Vec<u8>,
        /// Exact named space the profile must map to, if it is a standard combination.
        named: Option<ColorSpace>,
        primaries: Primaries,
        tf: fn(&TransferFunction) -> bool,
    }

    fn cases() -> Vec<Case> {
        let srgb_table = || sampled_curve(|x| ColorSpace::srgb().to_linear(x));
        vec![
            Case {
                name: "srgb_para",
                gamut: ColorSpace::srgb(),
                curve: srgb_para_tag(),
                named: Some(ColorSpace::srgb()),
                primaries: Primaries::Srgb,
                tf: |t| *t == TransferFunction::Srgb,
            },
            Case {
                name: "srgb_sampled_table",
                gamut: ColorSpace::srgb(),
                curve: srgb_table(),
                named: Some(ColorSpace::srgb()),
                primaries: Primaries::Srgb,
                tf: |t| *t == TransferFunction::Srgb,
            },
            Case {
                name: "srgb_linear",
                gamut: ColorSpace::srgb(),
                curve: gamma_tag(1.0),
                named: Some(ColorSpace::linear_srgb()),
                primaries: Primaries::Srgb,
                tf: |t| *t == TransferFunction::Linear,
            },
            Case {
                name: "display_p3_srgb_curve",
                gamut: ColorSpace::display_p3(),
                curve: srgb_para_tag(),
                named: Some(ColorSpace::display_p3()),
                primaries: ColorSpace::display_p3().primaries(),
                tf: |t| *t == TransferFunction::Srgb,
            },
            Case {
                name: "display_p3_gamma_2_2",
                gamut: ColorSpace::display_p3(),
                curve: gamma_tag(2.2),
                named: None,
                primaries: ColorSpace::display_p3().primaries(),
                tf: |t| matches!(t, TransferFunction::Gamma(v) if (v - 2.2).abs() < 0.01),
            },
            Case {
                name: "display_p3_linear",
                gamut: ColorSpace::display_p3(),
                curve: gamma_tag(1.0),
                named: None,
                primaries: ColorSpace::display_p3().primaries(),
                tf: |t| *t == TransferFunction::Linear,
            },
            Case {
                name: "adobe_rgb_gamma",
                gamut: ColorSpace::adobe_rgb(),
                curve: gamma_tag(2.1992188),
                named: Some(ColorSpace::adobe_rgb()),
                primaries: Primaries::AdobeRgb,
                tf: |t| matches!(t, TransferFunction::Gamma(_)),
            },
            Case {
                name: "adobe_rgb_para_type0",
                gamut: ColorSpace::adobe_rgb(),
                curve: para_tag(0, &[2.1992188]),
                named: Some(ColorSpace::adobe_rgb()),
                primaries: Primaries::AdobeRgb,
                tf: |t| matches!(t, TransferFunction::Gamma(_)),
            },
            Case {
                name: "adobe_rgb_srgb_curve",
                gamut: ColorSpace::adobe_rgb(),
                curve: srgb_para_tag(),
                named: None,
                primaries: Primaries::AdobeRgb,
                tf: |t| *t == TransferFunction::Srgb,
            },
            Case {
                name: "prophoto_gamma_1_8",
                gamut: prophoto(),
                curve: gamma_tag(1.8),
                named: Some(prophoto()),
                primaries: Primaries::ProPhotoRgb,
                tf: |t| matches!(t, TransferFunction::Gamma(_)),
            },
            Case {
                name: "prophoto_romm_para",
                gamut: prophoto(),
                curve: romm_para_tag(),
                named: Some(prophoto()),
                primaries: Primaries::ProPhotoRgb,
                tf: |t| matches!(t, TransferFunction::Gamma(_)),
            },
            Case {
                name: "prophoto_srgb_curve",
                gamut: prophoto(),
                curve: srgb_para_tag(),
                named: None,
                primaries: Primaries::ProPhotoRgb,
                tf: |t| *t == TransferFunction::Srgb,
            },
            Case {
                name: "prophoto_linear",
                gamut: prophoto(),
                curve: gamma_tag(1.0),
                named: None,
                primaries: Primaries::ProPhotoRgb,
                tf: |t| *t == TransferFunction::Linear,
            },
        ]
    }

    #[test]
    fn icc_standard_profiles_map_to_the_matching_named_space() {
        for c in cases() {
            let cs = rgb_profile(&c.gamut, c.curve.clone(), PrimaryPoints::D50)
                .to_color_space()
                .unwrap();
            assert_eq!(cs.primaries(), c.primaries, "{}: primaries", c.name);
            assert!(
                (c.tf)(&cs.transfer_function()),
                "{}: {:?}",
                c.name,
                cs.transfer_function()
            );
            match &c.named {
                Some(n) => assert_eq!(&cs, n, "{}", c.name),
                None => {
                    // Standard gamut with a non-standard curve: a custom space, never the named one.
                    for n in [
                        ColorSpace::srgb(),
                        ColorSpace::display_p3(),
                        ColorSpace::adobe_rgb(),
                        prophoto(),
                    ] {
                        assert_ne!(cs, n, "{}", c.name);
                    }
                }
            }
        }
    }

    #[test]
    fn icc_named_profiles_are_pairwise_distinct() {
        let all: Vec<ColorSpace> = [
            (ColorSpace::srgb(), srgb_para_tag()),
            (ColorSpace::display_p3(), srgb_para_tag()),
            (ColorSpace::adobe_rgb(), gamma_tag(2.1992188)),
            (prophoto(), gamma_tag(1.8)),
        ]
        .into_iter()
        .map(|(g, t)| {
            rgb_profile(&g, t, PrimaryPoints::D50)
                .to_color_space()
                .unwrap()
        })
        .collect();
        for i in 0..all.len() {
            for j in 0..all.len() {
                assert_eq!(i == j, all[i] == all[j], "{i} vs {j}");
            }
        }
    }

    #[test]
    fn icc_prophoto_keeps_its_d50_white_and_qt_matrix() {
        // ProPhoto RGB is defined relative to D50: the white point must stay D50 (not D65) and
        // the RGB -> XYZ matrix is the published ROMM RGB one.
        let cs = rgb_profile(&prophoto(), gamma_tag(1.8), PrimaryPoints::D50)
            .to_color_space()
            .unwrap();
        assert_eq!(cs.primary_points().white_point, PrimaryPoints::D50);
        let m = cs.rgb_to_xyz_matrix();
        let romm = [
            [0.7977, 0.1352, 0.0313],
            [0.2880, 0.7119, 0.0001],
            [0.0000, 0.0000, 0.8249],
        ];
        for r in 0..3 {
            for c in 0..3 {
                assert!(
                    (m[r][c] - romm[r][c]).abs() < 1e-3,
                    "[{r}][{c}] = {}",
                    m[r][c]
                );
            }
        }
        // The same profile with a non-standard curve keeps D50 colorants rather than being
        // mislabelled through a D65 comparison.
        let custom = rgb_profile(&prophoto(), gamma_tag(2.2), PrimaryPoints::D50)
            .to_color_space()
            .unwrap();
        assert_eq!(custom.primaries(), Primaries::ProPhotoRgb);
        assert_eq!(custom.primary_points().white_point, PrimaryPoints::D50);
    }

    #[test]
    fn icc_display_p3_matches_color_space_display_p3_exactly() {
        let cs = rgb_profile(
            &ColorSpace::display_p3(),
            srgb_para_tag(),
            PrimaryPoints::D50,
        )
        .to_color_space()
        .unwrap();
        let p3 = ColorSpace::display_p3();
        assert_eq!(cs.primaries(), p3.primaries());
        assert_eq!(cs.primary_points(), p3.primary_points());
        assert_eq!(cs.transfer_function(), p3.transfer_function());
        assert_eq!(cs.primary_points().white_point, PrimaryPoints::D65);
        // Same primaries label as a custom-curve P3 profile.
        let g22 = rgb_profile(
            &ColorSpace::display_p3(),
            gamma_tag(2.2),
            PrimaryPoints::D50,
        )
        .to_color_space()
        .unwrap();
        assert_eq!(g22.primaries(), p3.primaries());
    }

    #[test]
    fn named_spaces_use_qt_primaries_and_white_points() {
        // QColorSpace: sRGB / DCI-P3-D65 / Adobe RGB share D65 (0.3127, 0.3290); ProPhoto uses
        // D50 (0.3457, 0.3585).
        let p = |x, y| PointF { x, y };
        let table: [(ColorSpace, PointF, [PointF; 3]); 4] = [
            (
                ColorSpace::srgb(),
                p(0.3127, 0.3290),
                [p(0.64, 0.33), p(0.30, 0.60), p(0.15, 0.06)],
            ),
            (
                ColorSpace::display_p3(),
                p(0.3127, 0.3290),
                [p(0.68, 0.32), p(0.265, 0.69), p(0.15, 0.06)],
            ),
            (
                ColorSpace::adobe_rgb(),
                p(0.3127, 0.3290),
                [p(0.64, 0.33), p(0.21, 0.71), p(0.15, 0.06)],
            ),
            (
                prophoto(),
                p(0.3457, 0.3585),
                [p(0.7347, 0.2653), p(0.1596, 0.8404), p(0.0366, 0.0001)],
            ),
        ];
        for (cs, white, rgb) in table {
            let pts = cs.primary_points();
            assert_eq!(pts.white_point, white, "{}", cs.description());
            assert_eq!(
                [pts.red_point, pts.green_point, pts.blue_point],
                rgb,
                "{}",
                cs.description()
            );
            // RGB (1, 1, 1) must land exactly on the white point (Y = 1).
            let m = cs.rgb_to_xyz_matrix();
            let sum = |r: usize| m[r][0] + m[r][1] + m[r][2];
            let w = [white.x / white.y, 1.0, (1.0 - white.x - white.y) / white.y];
            for (r, &want) in w.iter().enumerate() {
                assert!(
                    (sum(r) - want).abs() < 1e-4,
                    "{} row {r}: {}",
                    cs.description(),
                    sum(r)
                );
            }
        }
        // sRGB -> XYZ (D65), IEC 61966-2-1.
        let m = ColorSpace::srgb().rgb_to_xyz_matrix();
        let srgb = [
            [0.4124, 0.3576, 0.1805],
            [0.2126, 0.7152, 0.0722],
            [0.0193, 0.1192, 0.9505],
        ];
        for r in 0..3 {
            for c in 0..3 {
                assert!(
                    (m[r][c] - srgb[r][c]).abs() < 1e-3,
                    "[{r}][{c}] = {}",
                    m[r][c]
                );
            }
        }
    }

    #[test]
    fn icc_profiles_convert_like_qt() {
        // Reference values from `QColorTransform::map` (PySide6 6.11.2): (source space,
        // input, output in sRGB). Qt's ProPhoto curve has a linear toe, so only mid-tones and
        // saturated colours are compared; tolerance covers Qt's 8-bit LUT rounding.
        let p3 = rgb_profile(
            &ColorSpace::display_p3(),
            srgb_para_tag(),
            PrimaryPoints::D50,
        );
        let adobe = rgb_profile(
            &ColorSpace::adobe_rgb(),
            gamma_tag(2.1992188),
            PrimaryPoints::D50,
        );
        let pro = rgb_profile(&prophoto(), gamma_tag(1.8), PrimaryPoints::D50);
        type Row<'a> = (&'a IccProfile, (u8, u8, u8), (u8, u8, u8));
        let rows: [Row; 8] = [
            (&p3, (200, 100, 50), (215, 93, 31)),
            (&p3, (30, 160, 220), (0, 163, 226)),
            (&p3, (128, 128, 128), (128, 128, 128)),
            (&adobe, (200, 100, 50), (227, 100, 42)),
            (&adobe, (30, 160, 220), (0, 161, 223)),
            (&adobe, (128, 128, 128), (129, 129, 129)),
            (&pro, (200, 100, 50), (255, 80, 47)),
            (&pro, (128, 128, 128), (146, 146, 146)),
        ];
        for (profile, input, expect) in rows {
            let cs = profile.to_color_space().unwrap();
            let (r, g, b, _) = cs
                .transformation_to_color_space(&ColorSpace::srgb())
                .map_rgba_u8(input.0, input.1, input.2, 255);
            for (got, want) in [(r, expect.0), (g, expect.1), (b, expect.2)] {
                assert!(
                    (got as i32 - want as i32).abs() <= 2,
                    "{} {input:?}: got {:?}, want {expect:?}",
                    cs.description(),
                    (r, g, b)
                );
            }
        }
    }

    #[test]
    fn icc_divergent_curves_are_applied_per_channel() {
        let gamut = ColorSpace::srgb();
        let profile = rgb_profile_curves(
            &gamut,
            [gamma_tag(2.2), gamma_tag(1.0), srgb_para_tag()],
            PrimaryPoints::D50,
        );
        let cs = profile.to_color_space().unwrap();
        assert_eq!(cs.primaries(), Primaries::Srgb);
        // Like QColorSpace, divergent curves report `Custom`; each channel keeps its own.
        assert_eq!(cs.transfer_function(), TransferFunction::Custom);
        let ch = cs.channel_transfer_functions();
        assert!(
            matches!(ch[0], TransferFunction::Gamma(g) if (g - 2.2).abs() < 0.01),
            "{ch:?}"
        );
        assert_eq!(ch[1], TransferFunction::Linear);
        assert_eq!(ch[2], TransferFunction::Srgb);
        assert!(!cs.is_linear());
        assert_ne!(cs, ColorSpace::srgb());

        // Decoding to linear sRGB primaries evaluates each channel's own curve.
        let to_lin = cs.transformation_to_color_space(&ColorSpace::linear_srgb());
        let near = |a: f32, b: f32| (a - b).abs() < 5e-3;
        let (r, _, _) = to_lin.map_rgb(0.5, 0.0, 0.0);
        assert!(near(r, 0.5f32.powf(2.2)), "{r}");
        let (_, g, _) = to_lin.map_rgb(0.0, 0.5, 0.0);
        assert!(near(g, 0.5), "{g}");
        let (_, _, b) = to_lin.map_rgb(0.0, 0.0, 0.5);
        assert!(near(b, ColorSpace::srgb().to_linear(0.5)), "{b}");

        // Encoding from linear sRGB applies the inverse of each channel's curve.
        let from_lin = ColorSpace::linear_srgb().transformation_to_color_space(&cs);
        let (r, _, _) = from_lin.map_rgb(0.25, 0.0, 0.0);
        assert!(near(r, 0.25f32.powf(1.0 / 2.2)), "{r}");
        let (_, g, _) = from_lin.map_rgb(0.0, 0.25, 0.0);
        assert!(near(g, 0.25), "{g}");
        let (_, _, b) = from_lin.map_rgb(0.0, 0.0, 0.25);
        assert!(near(b, ColorSpace::srgb().to_encoded(0.25)), "{b}");

        // The scalar API cannot describe divergent curves: it reports the identity.
        assert_eq!(cs.to_linear(0.5), 0.5);
        assert!((cs.to_linear_channel(0, 0.5) - 0.5f32.powf(2.2)).abs() < 1e-3);

        // Reference values from QColorSpace::fromIccProfile + QColorTransform::map (PySide6
        // 6.11.2) for the same divergent profile.
        type Rgb = (u8, u8, u8);
        let rows: [(ColorSpace, Rgb, Rgb); 5] = [
            (ColorSpace::srgb(), (200, 100, 50), (201, 168, 50)),
            (ColorSpace::srgb(), (64, 128, 192), (62, 188, 192)),
            (ColorSpace::srgb(), (0, 128, 0), (0, 188, 0)),
            (ColorSpace::linear_srgb(), (200, 100, 50), (149, 100, 8)),
            (ColorSpace::linear_srgb(), (64, 128, 192), (12, 128, 134)),
        ];
        for (dst, input, want) in rows {
            let (r, g, b, _) = cs
                .transformation_to_color_space(&dst)
                .map_rgba_u8(input.0, input.1, input.2, 255);
            let ok = |a: u8, w: u8| (a as i32 - w as i32).abs() <= 2;
            assert!(
                ok(r, want.0) && ok(g, want.1) && ok(b, want.2),
                "{input:?} -> {}: got {:?}, want {want:?}",
                dst.description(),
                (r, g, b)
            );
        }
    }

    #[test]
    fn icc_nearly_identical_curves_still_map_to_the_named_space() {
        // Three tables that differ in the last digits (not bit-identical, fitted gammas differ
        // by far less than 1e-4 relative) are one curve, not divergent per-channel curves.
        let tables = [2.2f32, 2.20005, 2.2001].map(|g| sampled_curve(|x| x.powf(g)));
        let profile = rgb_profile_curves(&ColorSpace::adobe_rgb(), tables, PrimaryPoints::D50);
        let fitted = [b"rTRC", b"gTRC", b"bTRC"].map(|s| profile.trc_tag(s).unwrap());
        assert!(
            fitted[0] != fitted[1] || fitted[1] != fitted[2],
            "{fitted:?}"
        );
        let cs = profile.to_color_space().unwrap();
        assert_eq!(cs, ColorSpace::adobe_rgb());

        // Clearly different gammas remain divergent.
        let tables = [2.2f32, 2.3, 2.4].map(|g| sampled_curve(|x| x.powf(g)));
        let cs = rgb_profile_curves(&ColorSpace::adobe_rgb(), tables, PrimaryPoints::D50)
            .to_color_space()
            .unwrap();
        assert_eq!(cs.transfer_function(), TransferFunction::Custom);
    }

    #[test]
    fn icc_equal_curves_stay_a_single_transfer_function() {
        // Identical tables on every channel are not "divergent".
        let t = sampled_curve(|x| x.powf(2.2));
        let cs = rgb_profile_curves(
            &ColorSpace::srgb(),
            [t.clone(), t.clone(), t],
            PrimaryPoints::D50,
        )
        .to_color_space()
        .unwrap();
        assert!(
            matches!(cs.transfer_function(), TransferFunction::Gamma(g) if (g - 2.2).abs() < 0.01)
        );
        let ch = cs.channel_transfer_functions();
        assert!(ch.iter().all(|c| *c == cs.transfer_function()));
    }

    fn malformed_curves() -> Vec<(&'static str, Vec<u8>)> {
        let mut huge = b"curv\0\0\0\0".to_vec();
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        let mut short = b"curv\0\0\0\0".to_vec();
        short.extend_from_slice(&4u32.to_be_bytes());
        short.extend_from_slice(&[0xFF, 0xFF]);
        vec![
            ("empty_tag", Vec::new()),
            ("unknown_type", b"mft2\0\0\0\0\0\0\0\0".to_vec()),
            ("curv_count_beyond_tag", huge),
            ("curv_table_truncated", short),
            ("curv_gamma_zero", gamma_tag(0.0)),
            (
                "para_unknown_function",
                para_tag(9, &[2.4, 1.0, 0.0, 0.0, 0.0]),
            ),
            ("para_gamma_zero", para_tag(3, &[0.0, 1.0, 0.0, 0.0, 0.0])),
            ("para_gamma_negative", para_tag(0, &[-2.0])),
            (
                "para_a_zero",
                para_tag(3, &[2.4, 0.0, 0.055, 1.0 / 12.92, 0.04045]),
            ),
            ("para_params_truncated", para_tag(4, &[2.4, 1.0, 0.0])),
            (
                "para_overflows_to_infinity",
                para_tag(3, &[30000.0, 30000.0, 0.0, 0.0, 0.5]),
            ),
        ]
    }

    #[test]
    fn icc_malformed_curves_have_a_defined_fallback() {
        let base = rgb_profile(
            &ColorSpace::display_p3(),
            srgb_para_tag(),
            PrimaryPoints::D50,
        );
        for (name, bad) in malformed_curves() {
            // Directly: an unusable curve parses to nothing instead of panicking.
            let one = build(b"GRAY", &[(*b"kTRC", bad.clone())]);
            assert!(one.trc_tag(b"kTRC").is_none(), "{name}");
            assert!(one.to_color_space().unwrap().is_linear(), "{name}");

            // Every channel unusable: the P3 gamut is kept with an sRGB curve.
            let all = with_trcs(
                &base,
                [Some(bad.clone()), Some(bad.clone()), Some(bad.clone())],
            );
            assert_eq!(
                all.to_color_space().unwrap(),
                ColorSpace::display_p3(),
                "{name}"
            );

            // One usable curve serves the channels whose own curve is unusable.
            let some = with_trcs(&base, [Some(bad.clone()), Some(gamma_tag(2.2)), Some(bad)]);
            let cs = some.to_color_space().unwrap();
            assert_eq!(
                cs.primaries(),
                ColorSpace::display_p3().primaries(),
                "{name}"
            );
            assert!(
                matches!(cs.transfer_function(), TransferFunction::Gamma(g) if (g - 2.2).abs() < 0.01),
                "{name}: {:?}",
                cs.transfer_function()
            );
            assert_eq!(
                cs.channel_transfer_functions()[0],
                cs.transfer_function(),
                "{name}"
            );
        }
        // No TRC tag at all.
        let none = with_trcs(&base, [None, None, None]);
        assert_eq!(none.to_color_space().unwrap(), ColorSpace::display_p3());
    }

    #[test]
    fn icc_divergent_and_malformed_curves_mix() {
        let base = rgb_profile(&ColorSpace::srgb(), srgb_para_tag(), PrimaryPoints::D50);
        // r valid, g malformed (borrows r's curve), b a different valid curve.
        let p = with_trcs(
            &base,
            [
                Some(gamma_tag(2.2)),
                Some(gamma_tag(0.0)),
                Some(gamma_tag(1.0)),
            ],
        );
        let cs = p.to_color_space().unwrap();
        assert_eq!(cs.transfer_function(), TransferFunction::Custom);
        let ch = cs.channel_transfer_functions();
        assert!(matches!(ch[0], TransferFunction::Gamma(g) if (g - 2.2).abs() < 0.01));
        assert_eq!(ch[1], ch[0]);
        assert_eq!(ch[2], TransferFunction::Linear);
        // Transforming with it never panics or yields non-finite output.
        for to in [ColorSpace::srgb(), ColorSpace::linear_srgb()] {
            let (r, g, b) = cs.transformation_to_color_space(&to).map_rgb(0.3, 0.6, 0.9);
            assert!(r.is_finite() && g.is_finite() && b.is_finite());
        }
    }

    #[test]
    fn icc_malformed_colorants_fall_back_to_srgb() {
        let good = rgb_profile(
            &ColorSpace::display_p3(),
            srgb_para_tag(),
            PrimaryPoints::D50,
        );
        let mut tags: Vec<([u8; 4], Vec<u8>)> = vec![
            (*b"gXYZ", good.tag(b"gXYZ").unwrap().to_vec()),
            (*b"bXYZ", good.tag(b"bXYZ").unwrap().to_vec()),
            (*b"rTRC", srgb_para_tag()),
        ];
        // All-zero red colorant: no chromaticity.
        let mut with_zero = tags.clone();
        with_zero.push((*b"rXYZ", xyz_tag([0.0, 0.0, 0.0])));
        assert_eq!(
            build(b"RGB ", &with_zero).to_color_space().unwrap(),
            ColorSpace::srgb()
        );
        // Truncated red colorant.
        tags.push((*b"rXYZ", good.tag(b"rXYZ").unwrap()[..12].to_vec()));
        assert_eq!(
            build(b"RGB ", &tags).to_color_space().unwrap(),
            ColorSpace::srgb()
        );
        // Missing colorants altogether.
        let bare = build(b"RGB ", &[(*b"rTRC", srgb_para_tag())]);
        assert_eq!(bare.to_color_space().unwrap(), ColorSpace::srgb());
    }
}
