//! Raster assertions over actual Metal captures, independent of painted-text records.
use std::{fs::File, io::BufReader, path::Path};

use serde_json::json;

#[derive(Clone, Copy)]
struct Region {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

struct Raster {
    width: u32,
    height: u32,
    channels: usize,
    pixels: Vec<u8>,
}

impl Raster {
    fn load(path: &Path) -> Self {
        let mut decoder = png::Decoder::new(BufReader::new(File::open(path).expect("GPU capture")));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().expect("PNG header");
        let mut pixels = vec![0; reader.output_buffer_size().expect("bounded PNG")];
        let info = reader.next_frame(&mut pixels).expect("PNG pixels");
        let channels = match info.color_type {
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            other => panic!("unexpected capture color type: {other:?}"),
        };
        pixels.truncate(info.buffer_size());
        Self {
            width: info.width,
            height: info.height,
            channels,
            pixels,
        }
    }

    fn crop(&self, region: Region) -> Vec<[u8; 3]> {
        assert!(region.x + region.width <= self.width);
        assert!(region.y + region.height <= self.height);
        let mut output = Vec::new();
        for y in region.y..region.y + region.height {
            for x in region.x..region.x + region.width {
                let index = (y * self.width + x) as usize * self.channels;
                output.push(self.pixels[index..index + 3].try_into().expect("RGB"));
            }
        }
        output
    }
}

#[derive(Clone, Copy)]
struct Grid {
    width: u32,
    height: u32,
}

impl Grid {
    fn load(directory: &Path) -> Self {
        let value: serde_json::Value = serde_json::from_reader(
            File::open(directory.join("pixel-geometry.json")).expect("geometry"),
        )
        .expect("geometry JSON");
        let scale = value["scale"].as_f64().expect("scale");
        Self {
            width: (value["cellWidth"].as_f64().expect("width") * scale).round() as u32,
            height: (value["cellHeight"].as_f64().expect("height") * scale).round() as u32,
        }
    }

    fn cell(self, row: u32, col: u32, columns: u32) -> Region {
        Region {
            x: col * self.width,
            y: row * self.height,
            width: columns * self.width,
            height: self.height,
        }
    }
}

fn energy(pixels: &[[u8; 3]]) -> f64 {
    pixels.iter().flatten().map(|value| f64::from(*value)).sum()
}

fn difference(left: &[[u8; 3]], right: &[[u8; 3]]) -> f64 {
    assert_eq!(left.len(), right.len());
    left.iter()
        .flatten()
        .zip(right.iter().flatten())
        .map(|(left, right)| f64::from(left.abs_diff(*right)))
        .sum::<f64>()
        / (left.len() * 3) as f64
}

fn near(pixel: &[u8; 3], color: [u8; 3]) -> bool {
    pixel
        .iter()
        .zip(color)
        .all(|(actual, expected)| actual.abs_diff(expected) <= 3)
}

fn color_count(pixels: &[[u8; 3]], color: [u8; 3]) -> usize {
    pixels.iter().filter(|pixel| near(pixel, color)).count()
}

fn faint_ratio(image: &Raster, grid: Grid, row: u32, columns: u32) -> f64 {
    let normal = energy(&image.crop(grid.cell(row, 0, columns)));
    let faint = energy(&image.crop(grid.cell(row, 4, columns)));
    assert!(normal > 1000., "reference glyph must actually rasterize");
    faint / normal
}

fn check_faint(image: &Raster, grid: Grid) -> (f64, f64) {
    let emoji_pixels = image.crop(grid.cell(0, 0, 2));
    assert!(
        emoji_pixels
            .iter()
            .filter(|[red, green, blue]| red.saturating_sub(*blue) > 40
                && green.saturating_sub(*blue) > 20)
            .count()
            > 10,
        "reference must be a rasterized color emoji, not monochrome fallback/tofu"
    );
    let emoji = faint_ratio(image, grid, 0, 2);
    let mono = faint_ratio(image, grid, 1, 1);
    // Permit both linear-light and sRGB targets without fixing glyph raster bytes.
    // Equality between paths catches a second alpha multiplication on mono.
    assert!(
        (0.4..0.8).contains(&emoji),
        "faint emoji ratio {emoji}: missing opacity"
    );
    assert!(
        (0.4..0.8).contains(&mono),
        "faint mono ratio {mono}: missing/double opacity"
    );
    assert!(
        (emoji - mono).abs() < 0.08,
        "emoji/mono dimming disagree: {emoji}/{mono}"
    );
    (emoji, mono)
}

fn check_colors(image: &Raster, grid: Grid) {
    let foreground = [240, 60, 30];
    let background = [20, 40, 160];
    let normal = image.crop(grid.cell(2, 0, 1));
    let inverse = image.crop(grid.cell(2, 4, 1));
    assert!(
        color_count(&normal, background) > normal.len() / 3,
        "true-color background missing"
    );
    assert!(
        color_count(&normal, foreground) > 2,
        "true-color glyph missing"
    );
    assert!(
        color_count(&inverse, foreground) > inverse.len() / 3,
        "inverse background missing"
    );
    assert!(
        color_count(&inverse, background) > 2,
        "inverse glyph missing"
    );
    let blank = image.crop(grid.cell(2, 8, 1));
    assert_eq!(
        color_count(&blank, background),
        blank.len(),
        "background-only cell must fill every pixel"
    );
}

fn band(region: Region, start: f32, end: f32) -> Region {
    let top = (region.height as f32 * start).floor() as u32;
    let bottom = (region.height as f32 * end).ceil() as u32;
    Region {
        y: region.y + top,
        height: bottom - top,
        ..region
    }
}

fn check_decorations(image: &Raster, grid: Grid) {
    assert_eq!(
        energy(&image.crop(grid.cell(3, 0, 1))),
        0.,
        "unstyled reference is blank"
    );
    for (col, start, end) in [
        (4, 0.7, 1.),
        (8, 0.6, 1.),
        (12, 0.4, 0.7),
        (16, 0., 0.2),
        (20, 0.6, 1.),
        (24, 0.7, 1.),
        (28, 0.7, 1.),
    ] {
        let energy = energy(&image.crop(band(grid.cell(3, col, 1), start, end)));
        assert!(
            energy > f64::from(grid.width) * 100.,
            "decoration at column {col} not rasterized in expected band"
        );
    }
    let single = energy(&image.crop(grid.cell(3, 4, 1)));
    let double = energy(&image.crop(grid.cell(3, 8, 1)));
    assert!(
        double > single * 1.5,
        "double underline lost its second rule"
    );
}

fn check_glyphs(image: &Raster, grid: Grid) {
    let wide = image.crop(grid.cell(5, 0, 2));
    let repeated = image.crop(grid.cell(5, 4, 2));
    assert!(energy(&wide) > 1000., "CJK fallback has no raster");
    assert!(
        difference(&wide, &repeated) < 1.,
        "wide-glyph raster drifts by column"
    );
    assert!(
        difference(
            &image.crop(grid.cell(1, 0, 1)),
            &image.crop(grid.cell(5, 6, 1))
        ) < 1.,
        "glyph after wide spacer is misplaced"
    );
    let combining = image.crop(grid.cell(6, 0, 1));
    let composed = image.crop(grid.cell(6, 4, 1));
    assert!(energy(&combining) > 1000., "combining glyph missing");
    assert!(
        difference(&combining, &composed) < 4.,
        "combining/composed glyph raster differs"
    );
    for col in [8, 12] {
        assert!(
            energy(&image.crop(grid.cell(6, col, 1))) > 500.,
            "Greek/math fallback absent at {col}"
        );
    }
}

fn check_cursors(directory: &Path, grid: Grid) {
    let region = grid.cell(4, 4, 1);
    let capture = |name| Raster::load(&directory.join(format!("pixels-{name}.png"))).crop(region);
    let block = capture("block");
    let hidden = capture("hidden");
    let hollow = Raster::load(&directory.join("pixels-hollow.png"));
    assert!(
        color_count(&block, [255; 3]) > block.len() * 9 / 10,
        "solid cursor missing"
    );
    assert_eq!(energy(&hidden), 0., "hidden cursor left pixels");
    let center = Region {
        x: region.x + 2,
        y: region.y + 2,
        width: region.width - 4,
        height: region.height - 4,
    };
    assert_eq!(
        energy(&hollow.crop(center)),
        0.,
        "hollow cursor filled interior"
    );
    assert!(
        energy(&hollow.crop(region)) > 1000.,
        "hollow outline missing"
    );
    assert!(
        energy(&capture("bar")) < energy(&block) * 0.4,
        "bar painted as block"
    );
    assert!(
        energy(&capture("underline")) < energy(&block) * 0.3,
        "underline painted as block"
    );
    check_cursor_bands(directory, grid);
}

fn check_cursor_bands(directory: &Path, grid: Grid) {
    let region = grid.cell(4, 4, 1);
    let bar = Raster::load(&directory.join("pixels-bar.png"));
    let underline = Raster::load(&directory.join("pixels-underline.png"));
    assert!(
        energy(&bar.crop(Region { width: 2, ..region })) > 1000.,
        "bar cursor left edge missing"
    );
    assert_eq!(
        energy(&bar.crop(Region {
            x: region.x + 3,
            width: region.width - 3,
            ..region
        })),
        0.,
        "bar cursor spills horizontally"
    );
    assert!(
        energy(&underline.crop(band(region, 0.8, 1.))) > 1000.,
        "underline cursor bottom missing"
    );
    assert_eq!(
        energy(&underline.crop(band(region, 0., 0.7))),
        0.,
        "underline cursor spills vertically"
    );
}

fn check_clipping(directory: &Path, grid: Grid) {
    let clipped = Raster::load(&directory.join("pixels-clipped.png"));
    let reference = Raster::load(&directory.join("pixels-underline.png"));
    let width = grid.width * 5;
    let height = (f64::from(grid.height) * 6.5).round() as u32;
    let inside = Region {
        x: 0,
        y: 0,
        width,
        height,
    };
    assert!(
        difference(&clipped.crop(inside), &reference.crop(inside)) < 1.,
        "clipping discarded or changed visible partial glyphs"
    );
    assert!(
        energy(&clipped.crop(Region {
            width: grid.width,
            ..grid.cell(0, 4, 2)
        })) > 300.,
        "partial emoji vanished"
    );
    for region in [
        Region {
            x: width,
            y: 0,
            width: clipped.width - width,
            height: clipped.height,
        },
        Region {
            x: 0,
            y: height,
            width,
            height: clipped.height - height,
        },
    ] {
        let outside = clipped.crop(region);
        assert_eq!(
            color_count(&outside, [250, 0, 250]),
            outside.len(),
            "glyph/background escaped surface clip into magenta sentinel"
        );
    }
}

pub fn verify(directory: &Path) {
    let grid = Grid::load(directory);
    let image = Raster::load(&directory.join("pixels-block.png"));
    let (emoji_ratio, mono_ratio) = check_faint(&image, grid);
    check_colors(&image, grid);
    check_decorations(&image, grid);
    check_glyphs(&image, grid);
    check_cursors(directory, grid);
    check_clipping(directory, grid);
    let receipt = json!({ "result": "pass", "source": "decoded native GPU PNG pixels", "faintEmojiRatio": emoji_ratio, "faintMonoRatio": mono_ratio, "checks": ["truecolor", "inverse", "background", "decorations", "cursor shapes", "faint emoji and mono", "wide spacing", "combining", "fallback presence", "clipped partial glyphs", "outside clip sentinel"] });
    std::fs::write(
        directory.join("pixel-receipt.json"),
        serde_json::to_vec_pretty(&receipt).expect("receipt JSON"),
    )
    .expect("write receipt");
    println!("{receipt}");
}
