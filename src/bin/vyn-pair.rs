//! `vyn-pair` — renders a `vynkor://pair` link (or any text) as a QR code.
//!
//! K-05: split out of `vyn device connect` (`src/cli/device.rs`), which only
//! prints the plain pairing link/token now. This binary owns zero pairing
//! logic — it just draws a QR from a string, terminal (UTF-8 block glyphs) or
//! SVG. That keeps the `qrcode` crate (and the ISO 18004 capacity table used
//! for the version-size hint) out of the core `vyn` binary's link graph.
//!
//! Usage:
//!   vyn device connect ... | vyn-pair        # pull the link out of the output
//!   vyn-pair 'vynkor://pair?z=1&d=...'       # or pass it directly
//!   vyn-pair --svg-out pair.svg < link.txt   # also write a scannable SVG

use std::io::Read as _;

use clap::Parser;
use qrcode::render::svg;
use qrcode::render::unicode::Dense1x2;
use qrcode::QrCode;

#[derive(Parser)]
#[command(
    name = "vyn-pair",
    about = "Render a vynkor pairing link as a QR code",
    version = env!("CARGO_PKG_VERSION")
)]
struct Args {
    /// The link/text to encode. Default: read from stdin (accepts either the
    /// bare link or the full `vyn device connect` output — the first
    /// `vynkor://` token, or a `ws(s)://`/`http(s)://` token, is extracted).
    link: Option<String>,

    /// Also write the QR to this path (SVG, opens in a browser).
    #[arg(long)]
    svg_out: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let link = match args.link {
        Some(l) => l,
        None => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            extract_link(&input).ok_or_else(|| {
                anyhow::anyhow!(
                    "no link found on stdin — pass it as an argument, or pipe \
                     `vyn device connect` output"
                )
            })?
        }
    };

    print_qr(&link)?;
    println!("\n{link}\n");
    println!("link {} chars (QR v{})", link.len(), qr_version(&link));

    if let Some(path) = args.svg_out {
        write_svg(&link, &path)?;
        eprintln!("QR written to {path}");
    }
    Ok(())
}

/// Pull the pairing link out of arbitrary text (e.g. the full `vyn device
/// connect` stdout, which has header/footer lines around it) — the first
/// whitespace-delimited token that looks like a URI, else the trimmed input.
fn extract_link(input: &str) -> Option<String> {
    input
        .split_whitespace()
        .find(|tok| {
            tok.starts_with("vynkor://")
                || tok.starts_with("ws://")
                || tok.starts_with("wss://")
                || tok.starts_with("http://")
                || tok.starts_with("https://")
        })
        .map(str::to_string)
        .or_else(|| {
            let trimmed = input.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
}

/// Approximate QR version for the printed size hint (byte capacity of
/// versions 1..=40 at ECC level L, numeric/alphanumeric ignored — we're
/// byte-mode).
fn qr_version(link: &str) -> usize {
    // byte capacities, ECC L, versions 1..40 (ISO/IEC 18004 tables)
    const CAPS: [usize; 40] = [
        17, 32, 53, 78, 106, 134, 154, 192, 230, 271, 321, 367, 425, 458, 520, 586, 644, 718, 792,
        858, 929, 1003, 1091, 1171, 1273, 1367, 1465, 1528, 1628, 1732, 1840, 1952, 2068, 2188,
        2303, 2431, 2563, 2699, 2809, 2953,
    ];
    let n = link.len();
    CAPS.iter()
        .position(|&cap| cap >= n)
        .map(|i| i + 1)
        .unwrap_or(41)
}

fn print_qr(link: &str) -> anyhow::Result<()> {
    let code = QrCode::new(link.as_bytes()).map_err(anyhow::Error::new)?;
    let image = code.render::<Dense1x2>().quiet_zone(true).build();
    println!("{image}");
    Ok(())
}

fn write_svg(link: &str, path: &str) -> anyhow::Result<()> {
    let code = QrCode::new(link.as_bytes()).map_err(anyhow::Error::new)?;
    let svg = code
        .render::<svg::Color>()
        .quiet_zone(true)
        .min_dimensions(512, 512)
        .build();
    std::fs::write(path, svg)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_version_estimates_byte_capacity() {
        assert_eq!(qr_version(&"a".repeat(10)), 1);
        assert_eq!(qr_version(&"a".repeat(100)), 5);
        assert_eq!(qr_version(&"a".repeat(3000)), 41);
    }

    #[test]
    fn extract_link_pulls_uri_token_out_of_surrounding_text() {
        let text = "Pairing link:\n\nvynkor://pair?z=1&d=abc\n\npaired device 'x'\n";
        assert_eq!(
            extract_link(text).as_deref(),
            Some("vynkor://pair?z=1&d=abc")
        );
    }

    #[test]
    fn extract_link_falls_back_to_trimmed_whole_input() {
        assert_eq!(
            extract_link("  just-some-token  \n").as_deref(),
            Some("just-some-token")
        );
    }

    #[test]
    fn extract_link_none_on_empty_input() {
        assert_eq!(extract_link("   \n  "), None);
    }
}
