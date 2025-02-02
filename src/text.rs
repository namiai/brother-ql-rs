//! Easy-to-use text and image compositing and rasterization for use with Brother QL printers

use crate::printer::constants::Label;
use image::{DynamicImage, Luma};
use rusttype::{Font, Point, Scale};
use std::fs;
use std::path::PathBuf;

type XY<T> = Point<T>;

fn calc_text_width(glyphs: &[rusttype::PositionedGlyph]) -> u32 {
    let min_x = glyphs
        .first()
        .map(|g| g.pixel_bounding_box().unwrap().min.x)
        .unwrap();
    let max_x = glyphs
        .last()
        .map(|g| g.pixel_bounding_box().unwrap().max.x)
        .unwrap();
    (max_x - min_x) as u32
}

struct ResizedText<'a> {
    rendered_size: XY<u32>,
    glyphs: Vec<rusttype::PositionedGlyph<'a>>,
}
impl<'a> ResizedText<'a> {
    pub fn create<'b>(font: &'a Font, text: &'b str, max_width: u32, max_font_size: f32) -> Self {
        let mut font_size = max_font_size.ceil(); // Max possible font size
        let rendered_size;
        // Scale the font size down until it all fits length-wise
        let glyphs = loop {
            let scale = Scale::uniform(font_size);
            let v_metrics = font.v_metrics(scale);
            let glyphs: Vec<_> = font
                .layout(
                    text,
                    scale,
                    Point {
                        x: 0.0,
                        y: v_metrics.ascent,
                    },
                )
                .collect();

            let width = calc_text_width(&glyphs);
            if width < max_width {
                let height = (v_metrics.ascent - v_metrics.descent).ceil() as u32;
                rendered_size = XY {
                    x: width,
                    y: height,
                };
                break glyphs;
            }
            font_size -= 1.0;
        };

        Self {
            rendered_size,
            glyphs,
        }
    }
}

fn draw_glyphs(
    image: &mut image::GrayImage,
    glyphs: &[rusttype::PositionedGlyph],
    offset: XY<i32>,
    invert: bool,
) {
    for glyph in glyphs {
        if let Some(bounding_box) = glyph.pixel_bounding_box() {
            // Draw the glyph into the image per-pixel by using the draw closure
            glyph.draw(|x, y, v| {
                let color = if invert {
                    (255.0 * v) as u8
                } else {
                    255 - (255.0 * v) as u8
                };

                image.put_pixel(
                    // Offset the position by the glyph bounding box
                    (x as i32 + bounding_box.min.x + offset.x) as u32,
                    (y as i32 + bounding_box.min.y + offset.y) as u32,
                    // Turn the coverage into an alpha value
                    Luma([color]),
                )
            });
        }
    }
}

pub fn image_to_raster_lines(image: &image::GrayImage, width: u32) -> Vec<[u8; 90]> {
    let width = width as usize;
    let line_count = image.len() / width;

    // We need to sidescan this generated image for the printer
    let mut lines = Vec::with_capacity(width);
    for c in 0..width {
        let mut line = [0; 90]; // Always 90 for regular sized printers like the QL-700 (with a 0x00 byte to start)
        let mut line_byte = 1;
        // Bit index counts backwards
        // First nibble (bits 7 through 4) in the second byte is blank
        let mut line_bit_index: i8 = 3;
        for r in 0..line_count {
            line_bit_index -= 1;
            if line_bit_index < 0 {
                line_byte += 1;
                line_bit_index += 8;
            }
            image.get_pixel(0, 0);
            let luma_pixel = image.get_pixel(c as u32, r as u32); // + 3 was here in TS code -- not sure if needed
            let value: u8 = if luma_pixel[0] > 0xFF / 2 { 0 } else { 1 };
            line[line_byte] |= value << line_bit_index;
        }
        lines.push(line);
    }
    lines
}

/// Easily convert text into a raster image that can be printed by a `ThermalPrinter`
pub struct TextRasterizer {
    label: Label,
    font_path: PathBuf,
    second_row_image: Option<PathBuf>,
}
impl TextRasterizer {
    /// The text rasterizer needs to know the loaded label media currently in the printer in order to resize and
    /// shift the text content accordingly so that it will fit.
    pub fn new(label: Label, font_path: PathBuf) -> Self {
        Self {
            label,
            font_path,
            second_row_image: None,
        }
    }
    /// Some types of label media (e.g. 12mm continuous tape) are wider than specified. Use this method to draw
    /// an image onto this second, normally out-of-bounds part of the tape. The bottom portion of the tape
    /// is usually pre-scored from the top part so consider this a way to make "bonus" labels with the same
    /// amount of physical tape.
    pub fn set_second_row_image(&mut self, path: PathBuf) {
        self.second_row_image = Some(path);
    }
    /// Transforms text into a raster image ready to send to the `ThermalPrinter`. Typically, the text will appear
    /// as black on a white background. Enable the `invert` flag to print white text on a black background. Note that
    /// since the label is white, a faint border of white will still surround the label in areas that the printer
    /// cannot print the black background.
    pub fn rasterize(
        &self,
        text: &str,
        secondary_text: Option<&str>,
        font_scale: f32,
        invert: bool,
    ) -> Vec<[u8; 90]> {
        let font_data = fs::read(&self.font_path).expect("Invalid font path");
        let font: Font<'static> = Font::from_bytes(font_data).unwrap();

        let mut length = 750;
        let mut width;
        let mut secondary_width = 0;

        if self.label.tape_size.1 == 0 {
            // Continuous tape
            width = self.label.dots_printable.0 + self.label.right_margin as u32;

            if self.label.tape_size.0 == 12 {
                // 12mm label seems to need this for some reason
                width += 25;
                // 12mm labels have a second label below the primary that can actually be used
                if self.second_row_image.is_some() {
                    secondary_width = 170;
                }
            }
        } else {
            // Die cut labels
            width = self.label.dots_printable.0 + self.label.right_margin as u32;
            length = self.label.dots_printable.1;
        }

        let mut image = DynamicImage::new_luma8(length, width + secondary_width).to_luma8();
        // Set image background
        for (_x, y, pixel) in image.enumerate_pixels_mut() {
            if invert {
                let top_label_size =
                    self.label.dots_printable.0 + self.label.right_margin as u32 + 15;
                if y > top_label_size {
                    *pixel = Luma([255]); // Set to white
                } else {
                    *pixel = Luma([0]); // Set to black
                }
            } else {
                *pixel = Luma([255]); // Set to white
            }
        }

        match secondary_text {
            Some(secondary_text) => {
                let primary = ResizedText::create(&font, text, length, 90.0 * font_scale);
                let secondary =
                    ResizedText::create(&font, secondary_text, length, 35.0 * font_scale);

                let primary_offset = XY {
                    x: (length as i32 / 2) - (primary.rendered_size.x as i32 / 2),
                    y: (width as i32 / 2) - (primary.rendered_size.y as i32 / 2) - 25,
                };
                let secondary_offset = XY {
                    x: (length as i32 / 2) - (secondary.rendered_size.x as i32 / 2),
                    y: (width as i32 / 1) - (secondary.rendered_size.y as i32 / 2) - 20,
                };
                draw_glyphs(&mut image, &primary.glyphs, primary_offset, invert);
                draw_glyphs(&mut image, &secondary.glyphs, secondary_offset, invert);
            }
            None => {
                let primary = ResizedText::create(&font, text, length, 125.0 * font_scale);

                let offset = XY {
                    x: (length as i32 / 2) - (primary.rendered_size.x as i32 / 2) - 5,
                    y: (width as i32 / 2) - (primary.rendered_size.y as i32 / 2),
                };

                draw_glyphs(&mut image, &primary.glyphs, offset, invert);
            }
        }

        if let Some(image_path) = &self.second_row_image {
            let overlay = image::open(image_path).unwrap().to_luma8();

            let top_margin = 15;
            let ratio = overlay.width() as f32 / overlay.height() as f32;

            let mut new_width: u32 = length;
            let mut new_height: u32 = (new_width as f32 / ratio) as u32;
            if new_height > secondary_width - top_margin {
                new_height = secondary_width - top_margin;
                new_width = (new_height as f32 * ratio) as u32;
            }
            let resized = image::imageops::resize(
                &overlay,
                new_width,
                new_height,
                image::imageops::FilterType::Triangle,
            );
            image::imageops::overlay(&mut image, &resized, (length - new_width) / 2, width);
        }

        // Save the image to a png file if debug mode is enabled
        if cfg!(debug_assertions) {
            image.save("render.png").unwrap();
        }
        image_to_raster_lines(&image, length)
    }
}
