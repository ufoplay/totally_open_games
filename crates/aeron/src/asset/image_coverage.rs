//! Port of `src/asset/image_coverage.c` (declared in `include/aeron/image.h`).

/// A rectangle of occupied pixels.
#[doc(alias = "AeronImageCoverageRect")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCoverageRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Disjoint rectangles covering every occupied 4×4 alpha cell of an image.
#[doc(alias("AeronImageCoverage", "Aeron_ImageFreeCoverage"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageCoverage {
    pub rects: Vec<ImageCoverageRect>,
    pub width: u16,
    pub height: u16,
}

/// Groups occupied 4×4 alpha cells of an RGBA8 image into disjoint rectangles. Geometry skips empty image
/// regions; the sampled texture still supplies exact pixel coverage.
///
/// Returns `None` for empty or oversized dimensions, or when `rgba` is shorter than `width * height * 4`.
#[doc(alias = "Aeron_ImageBuildCoverageRgba8")]
pub fn image_build_coverage_rgba8(
    rgba: &[u8],
    width: usize,
    height: usize,
) -> Option<ImageCoverage> {
    if width == 0 || height == 0 || width > usize::from(u16::MAX) || height > usize::from(u16::MAX)
    {
        return None;
    }
    if rgba.len() < width.checked_mul(height)?.checked_mul(4)? {
        return None;
    }
    let columns = width.div_ceil(4);
    let rows = height.div_ceil(4);
    let mut cells = vec![false; columns * rows];
    for cell_y in 0..rows {
        for cell_x in 0..columns {
            let occupied = (cell_y * 4..height.min(cell_y * 4 + 4)).any(|y| {
                (cell_x * 4..width.min(cell_x * 4 + 4)).any(|x| rgba[(y * width + x) * 4 + 3] != 0)
            });
            cells[cell_y * columns + cell_x] = occupied;
        }
    }

    let mut rects = Vec::new();
    for y in 0..rows {
        let mut x = 0;
        while x < columns {
            if !cells[y * columns + x] {
                x += 1;
                continue;
            }
            let mut run_width = 1;
            while x + run_width < columns && cells[y * columns + x + run_width] {
                run_width += 1;
            }
            let mut run_height = 1;
            while y + run_height < rows
                && cells[(y + run_height) * columns + x..][..run_width]
                    .iter()
                    .all(|&cell| cell)
            {
                run_height += 1;
            }
            for row in 0..run_height {
                cells[(y + row) * columns + x..][..run_width].fill(false);
            }
            rects.push(ImageCoverageRect {
                x: (x * 4) as i32,
                y: (y * 4) as i32,
                width: ((run_width * 4).min(width - x * 4)) as i32,
                height: ((run_height * 4).min(height - y * 4)) as i32,
            });
            x += run_width;
        }
    }
    Some(ImageCoverage {
        rects,
        width: width as u16,
        height: height as u16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_cells_into_rectangles() {
        // 10×6: an opaque pixel in cells (0,0), (1,0), (0,1), (1,1) and one in cell (2,1).
        let (width, height) = (10, 6);
        let mut rgba = vec![0u8; width * height * 4];
        let mut set = |x: usize, y: usize| rgba[(y * width + x) * 4 + 3] = 255;
        set(0, 0);
        set(5, 0);
        set(1, 4);
        set(7, 5);
        set(9, 5);
        let coverage = image_build_coverage_rgba8(&rgba, width, height).unwrap();
        assert_eq!(
            coverage.rects,
            [
                ImageCoverageRect {
                    x: 0,
                    y: 0,
                    width: 8,
                    height: 6
                },
                ImageCoverageRect {
                    x: 8,
                    y: 4,
                    width: 2,
                    height: 2
                },
            ]
        );
        assert!(image_build_coverage_rgba8(&rgba, 0, 6).is_none());
        assert!(image_build_coverage_rgba8(&rgba[..10], width, height).is_none());
    }
}
