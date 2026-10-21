use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub alpha: u8,
}

impl Default for Color {
    fn default() -> Self {
        Self {
            r: 0,
            g: 0,
            b: 0,
            alpha: 255,
        }
    }
}

impl Color {
    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        alpha: 255,
    };
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        alpha: 255,
    };
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        alpha: 0,
    };

    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim().to_ascii_lowercase();
        if s.is_empty() {
            return Some(Self::BLACK);
        }
        if let Some(hex) = s.strip_prefix('#') {
            return Self::from_hex(hex);
        }
        if let Some(inner) = s.strip_prefix("rgba(").and_then(|v| v.strip_suffix(')')) {
            let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            if parts.len() == 4 {
                let r = parse_channel(parts[0])?;
                let g = parse_channel(parts[1])?;
                let b = parse_channel(parts[2])?;
                let a = parse_alpha(parts[3])?;
                return Some(Self { r, g, b, alpha: a });
            }
        }
        if let Some(inner) = s.strip_prefix("rgb(").and_then(|v| v.strip_suffix(')')) {
            let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
            if parts.len() == 3 {
                return Some(Self {
                    r: parse_channel(parts[0])?,
                    g: parse_channel(parts[1])?,
                    b: parse_channel(parts[2])?,
                    alpha: 255,
                });
            }
        }
        match s.as_str() {
            "black" => Some(Self::BLACK),
            "white" => Some(Self::WHITE),
            "transparent" => Some(Self::TRANSPARENT),
            "red" => Some(Self {
                r: 255,
                g: 0,
                b: 0,
                alpha: 255,
            }),
            "green" => Some(Self {
                r: 0,
                g: 128,
                b: 0,
                alpha: 255,
            }),
            "lime" => Some(Self {
                r: 0,
                g: 255,
                b: 0,
                alpha: 255,
            }),
            "blue" => Some(Self {
                r: 0,
                g: 0,
                b: 255,
                alpha: 255,
            }),
            "yellow" => Some(Self {
                r: 255,
                g: 255,
                b: 0,
                alpha: 255,
            }),
            "cyan" | "aqua" => Some(Self {
                r: 0,
                g: 255,
                b: 255,
                alpha: 255,
            }),
            "magenta" | "fuchsia" => Some(Self {
                r: 255,
                g: 0,
                b: 255,
                alpha: 255,
            }),
            "gray" | "grey" => Some(Self {
                r: 128,
                g: 128,
                b: 128,
                alpha: 255,
            }),
            "silver" => Some(Self {
                r: 192,
                g: 192,
                b: 192,
                alpha: 255,
            }),
            "maroon" => Some(Self {
                r: 128,
                g: 0,
                b: 0,
                alpha: 255,
            }),
            "olive" => Some(Self {
                r: 128,
                g: 128,
                b: 0,
                alpha: 255,
            }),
            "navy" => Some(Self {
                r: 0,
                g: 0,
                b: 128,
                alpha: 255,
            }),
            "purple" => Some(Self {
                r: 128,
                g: 0,
                b: 128,
                alpha: 255,
            }),
            "teal" => Some(Self {
                r: 0,
                g: 128,
                b: 128,
                alpha: 255,
            }),
            "orange" => Some(Self {
                r: 255,
                g: 165,
                b: 0,
                alpha: 255,
            }),
            _ => None,
        }
    }

    fn from_hex(hex: &str) -> Option<Self> {
        let expand = |c: char| -> Option<u8> {
            let v = c.to_digit(16)? as u8;
            Some(v * 17)
        };
        match hex.len() {
            3 => {
                let mut chars = hex.chars();
                Some(Self {
                    r: expand(chars.next()?)?,
                    g: expand(chars.next()?)?,
                    b: expand(chars.next()?)?,
                    alpha: 255,
                })
            }
            4 => {
                let mut chars = hex.chars();
                Some(Self {
                    r: expand(chars.next()?)?,
                    g: expand(chars.next()?)?,
                    b: expand(chars.next()?)?,
                    alpha: expand(chars.next()?)?,
                })
            }
            6 => Some(Self {
                r: u8::from_str_radix(&hex[0..2], 16).ok()?,
                g: u8::from_str_radix(&hex[2..4], 16).ok()?,
                b: u8::from_str_radix(&hex[4..6], 16).ok()?,
                alpha: 255,
            }),
            8 => Some(Self {
                r: u8::from_str_radix(&hex[0..2], 16).ok()?,
                g: u8::from_str_radix(&hex[2..4], 16).ok()?,
                b: u8::from_str_radix(&hex[4..6], 16).ok()?,
                alpha: u8::from_str_radix(&hex[6..8], 16).ok()?,
            }),
            _ => None,
        }
    }
}

fn parse_channel(value: &str) -> Option<u8> {
    if let Some(percent) = value.strip_suffix('%') {
        let p: f64 = percent.parse().ok()?;
        return Some((p.clamp(0.0, 100.0) * 2.55).round() as u8);
    }
    let v: f64 = value.parse().ok()?;
    Some(v.clamp(0.0, 255.0).round() as u8)
}

fn parse_alpha(value: &str) -> Option<u8> {
    if let Some(percent) = value.strip_suffix('%') {
        let p: f64 = percent.parse().ok()?;
        return Some((p.clamp(0.0, 100.0) * 2.55).round() as u8);
    }
    let v: f64 = value.parse().ok()?;
    if v <= 1.0 {
        Some((v.clamp(0.0, 1.0) * 255.0).round() as u8)
    } else {
        Some(v.clamp(0.0, 255.0).round() as u8)
    }
}

impl std::fmt::Display for Color {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "#{:02x}{:02x}{:02x}{:02x}",
            self.r, self.g, self.b, self.alpha
        )
    }
}
