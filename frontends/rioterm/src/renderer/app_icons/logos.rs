use resvg::{tiny_skia, usvg};
use rio_backend::sugarloaf::Sugarloaf;

#[derive(Debug, PartialEq, Eq)]
pub struct Logo {
    pub name: &'static str,
    svg: &'static str,
    tint: bool,
}

macro_rules! logos {
    ($($name:literal: $tint:literal),* $(,)?) => {
        pub const LOGOS: &[Logo] = &[$(Logo {
            name: $name,
            svg: include_str!(concat!("logos/", $name, ".svg")),
            tint: $tint,
        }),*];
    };
}

logos! {
    "openai": true,
    "claude": true,
    "neovim": false,
    "helix": false,
    "superfile": false,
    "television": false,
    "goose": true,
    "jj": false,
    "htop": false,
    "gemini": false,
    "copilot": true,
    "nu": false,
    "opencode": true,
    "zed": true,
    "bash": false,
    "zsh": false,
    "podman": false,
    "julia": false,
    "dotnet": false,
    "bat": false,
    "terraform": false,
    "ansible": true,
    "helm": true,
    "nix": false,
    "postgres": false,
    "redis": false,
    "mysql": false,
    "kubernetes": false,
    "vim": false,
    "git": false,
    "docker": false,
    "python": false,
    "node": false,
    "rust": true,
    "go": false,
    "lua": true,
    "ruby": false,
    "php": false,
}

fn raster(logo: &Logo, size: u16) -> Vec<u8> {
    let tree = usvg::Tree::from_str(logo.svg, &usvg::Options::default())
        .expect("valid bundled SVG");
    let source = tree.size();
    let scale = size as f32 / source.width().max(source.height());
    let transform = tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        (size as f32 - source.width() * scale) / 2.0,
        (size as f32 - source.height() * scale) / 2.0,
    );
    let mut pixmap =
        tiny_skia::Pixmap::new(size as u32, size as u32).expect("bounded icon size");
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut pixels = pixmap.take();
    if logo.tint {
        for pixel in pixels.chunks_exact_mut(4) {
            let alpha = pixel[3];
            pixel[..3].fill(alpha);
        }
    }
    pixels
}

pub fn draw(
    sugarloaf: &mut Sugarloaf,
    logo: &'static Logo,
    cx: f32,
    cy: f32,
    size: f32,
    color: [f32; 4],
) -> bool {
    let tint = if logo.tint {
        color
    } else {
        [1.0, 1.0, 1.0, color[3]]
    };
    sugarloaf.text_mut().draw_icon(
        logo.name,
        cx,
        cy,
        size,
        tint.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8),
        |size| raster(logo, size),
    )
}
