//! Saving the window as an image, for self-checks and bug reports.

use std::io::BufWriter;
use std::path::Path;

use egui::ColorImage;

/// Writes `image` to `path` as an 8-bit RGBA PNG.
pub fn save_png(image: &ColorImage, path: &Path) -> std::io::Result<()> {
    let [width, height] = image.size;
    let file = std::fs::File::create(path)?;

    let mut encoder = png::Encoder::new(BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;

    let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        bytes.extend_from_slice(&pixel.to_array());
    }
    writer.write_image_data(&bytes)?;
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_image_reads_back_with_its_size_and_pixels() {
        let rgba = [0x12, 0x34, 0x56, 0xff, 0xf0, 0xe1, 0xd2, 0xff];
        let image = ColorImage::from_rgba_unmultiplied([2, 1], &rgba);
        let dir =
            std::env::temp_dir().join(format!("embdbg-screenshot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shot.png");
        save_png(&image, &path).unwrap();

        let decoder = png::Decoder::new(std::fs::File::open(&path).unwrap());
        let mut reader = decoder.read_info().unwrap();
        let (width, height) = (reader.info().width, reader.info().height);
        let mut buffer = vec![0; reader.output_buffer_size()];
        let decoded = reader.next_frame(&mut buffer).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!((width, height), (2, 1));
        assert_eq!(buffer[..decoded.buffer_size()], rgba[..]);
    }
}
