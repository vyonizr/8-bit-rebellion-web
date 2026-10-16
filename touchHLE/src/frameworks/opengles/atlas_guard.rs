/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Keep texture samples inside packed image rectangles during subpixel motion.
use crate::gles::gles11_raw::types::{GLenum, GLint, GLvoid};
use crate::gles::{gles11_raw as gl, GLES};
use std::collections::HashMap;

/// Keep samples inside the packed image. Linear filtering needs half a texel;
/// nearest filtering needs only a small raster-rounding guard. A half-texel
/// inset with nearest filtering would compress the interior sample grid and
/// make source-pixel widths uneven at integer render scales.
fn sampling_margin(magnification: GLint) -> f32 {
    if magnification == gl::NEAREST as GLint {
        1.0 / 64.0
    } else {
        0.5
    }
}

fn guard_quad(uv: &mut [f32; 8], dimensions: [i32; 2], margin: f32) -> bool {
    if dimensions.iter().any(|&v| v <= 0) {
        return false;
    }
    let original = *uv;
    let mut bounds = [[f32::INFINITY, f32::NEG_INFINITY]; 2];
    for (i, &value) in original.iter().enumerate() {
        let pixel = value * dimensions[i % 2] as f32;
        if !value.is_finite()
            || !(0.0..=1.0).contains(&value)
            || (pixel - pixel.round()).abs() > 0.001
        {
            return false;
        }
        bounds[i % 2][0] = bounds[i % 2][0].min(value);
        bounds[i % 2][1] = bounds[i % 2][1].max(value);
    }
    let mut corners = 0;
    for pair in original.chunks_exact(2) {
        let mut corner = 0;
        for axis in 0..2 {
            if pair[axis] == bounds[axis][1] {
                corner |= 1 << axis;
            } else if pair[axis] != bounds[axis][0] {
                return false;
            }
        }
        corners |= 1 << corner;
    }
    if corners != 15
        || (0..2)
            .any(|axis| (bounds[axis][1] - bounds[axis][0]) * (dimensions[axis] as f32) < 0.999)
    {
        return false;
    }
    for (i, value) in uv.iter_mut().enumerate() {
        let guard = margin / dimensions[i % 2] as f32;
        *value = original[i]
            + if original[i] == bounds[i % 2][0] {
                guard
            } else {
                -guard
            };
    }
    true
}

pub(super) struct Coordinates {
    values: Vec<f32>,
    original: *const GLvoid,
    stride: GLint,
}
impl Coordinates {
    pub(super) unsafe fn apply(&self, gles: &mut dyn GLES) {
        gles.TexCoordPointer(2, gl::FLOAT, 0, self.values.as_ptr().cast());
    }
    pub(super) unsafe fn restore(&self, gles: &mut dyn GLES) {
        gles.TexCoordPointer(2, gl::FLOAT, self.stride, self.original);
    }
}

unsafe fn integer(gles: &mut dyn GLES, name: GLenum) -> GLint {
    let mut value = 0;
    gles.GetIntegerv(name, &mut value);
    value
}

/// Nearest magnification may coexist with linear minification. Use the small
/// guard only when the observed 2D quad actually magnifies its source pixels.
unsafe fn magnifies_quad(gles: &mut dyn GLES, start: usize, uv: &[f32; 8], dims: [i32; 2]) -> bool {
    if integer(gles, gl::VERTEX_ARRAY_BUFFER_BINDING) != 0
        || integer(gles, gl::VERTEX_ARRAY_SIZE) != 2
        || integer(gles, gl::VERTEX_ARRAY_TYPE) != gl::FLOAT as GLint
    {
        return false;
    }
    let stride = integer(gles, gl::VERTEX_ARRAY_STRIDE);
    if stride < 0 || stride != 0 && stride < 8 {
        return false;
    }
    let step = if stride == 0 { 8 } else { stride as usize };
    let mut pointer = std::ptr::null();
    gles.GetPointerv(gl::VERTEX_ARRAY_POINTER, &mut pointer);
    if pointer.is_null() {
        return false;
    }
    let mut model = [0.0; 16];
    let mut projection = [0.0; 16];
    let mut viewport = [0; 4];
    gles.GetFloatv(gl::MODELVIEW_MATRIX, model.as_mut_ptr());
    gles.GetFloatv(gl::PROJECTION_MATRIX, projection.as_mut_ptr());
    gles.GetIntegerv(gl::VIEWPORT, viewport.as_mut_ptr());
    // Only affine 2D transforms: unfamiliar perspective paths retain the
    // conservative linear-filter guard.
    if model[3] != 0.0
        || model[7] != 0.0
        || model[15] != 1.0
        || projection[3] != 0.0
        || projection[7] != 0.0
        || projection[11] != 0.0
        || projection[15] != 1.0
    {
        return false;
    }
    let mut screen = [[0.0; 2]; 4];
    for (i, xy) in screen.iter_mut().enumerate() {
        let vertex = pointer.cast::<u8>().add((start + i) * step).cast::<f32>();
        let x = vertex.read_unaligned();
        let y = vertex.add(1).read_unaligned();
        let view = [
            model[0] * x + model[4] * y + model[12],
            model[1] * x + model[5] * y + model[13],
            model[2] * x + model[6] * y + model[14],
        ];
        for axis in 0..2 {
            xy[axis] = (projection[axis] * view[0]
                + projection[4 + axis] * view[1]
                + projection[8 + axis] * view[2]
                + projection[12 + axis])
                * viewport[2 + axis] as f32
                * 0.5;
        }
    }
    magnifies_rectangle(&screen, uv, dims)
}

fn magnifies_rectangle(screen: &[[f32; 2]; 4], uv: &[f32; 8], dims: [i32; 2]) -> bool {
    let u = (1..4).find(|&i| uv[i * 2] != uv[0] && uv[i * 2 + 1] == uv[1]);
    let v = (1..4).find(|&i| uv[i * 2] == uv[0] && uv[i * 2 + 1] != uv[1]);
    let (Some(u), Some(v)) = (u, v) else {
        return false;
    };
    let a = [screen[u][0] - screen[0][0], screen[u][1] - screen[0][1]];
    let b = [screen[v][0] - screen[0][0], screen[v][1] - screen[0][1]];
    let area = (a[0] * b[1] - a[1] * b[0]).abs();
    let width = (uv[u * 2] - uv[0]).abs() * dims[0] as f32;
    let height = (uv[v * 2 + 1] - uv[1]).abs() * dims[1] as f32;
    area.is_finite()
        && area > 0.0
        && area >= width * b[0].hypot(b[1])
        && area >= height * a[0].hypot(a[1])
}

/// Only the game's observed client-array quad paths are supported. Buffer
/// offsets, transformed/repeating coordinates and unfamiliar layouts stay intact.
pub(super) unsafe fn prepare(
    gles: &mut dyn GLES,
    textures: &HashMap<u32, [i32; 2]>,
    mode: GLenum,
    first: i32,
    count: i32,
    elements: Option<(GLenum, *const GLvoid)>,
) -> Option<Coordinates> {
    if gles.IsEnabled(gl::TEXTURE_2D) == 0
        || gles.IsEnabled(gl::TEXTURE_COORD_ARRAY) == 0
        || integer(gles, gl::ACTIVE_TEXTURE) != gl::TEXTURE0 as i32
        || integer(gles, gl::CLIENT_ACTIVE_TEXTURE) != gl::TEXTURE0 as i32
        || integer(gles, gl::TEXTURE_COORD_ARRAY_BUFFER_BINDING) != 0
        || integer(gles, gl::ELEMENT_ARRAY_BUFFER_BINDING) != 0
        || integer(gles, gl::TEXTURE_COORD_ARRAY_SIZE) != 2
        || integer(gles, gl::TEXTURE_COORD_ARRAY_TYPE) != gl::FLOAT as i32
    {
        return None;
    }
    let dimensions = *textures.get(&(integer(gles, gl::TEXTURE_BINDING_2D) as u32))?;
    let mut matrix = [0.0; 16];
    gles.GetFloatv(gl::TEXTURE_MATRIX, matrix.as_mut_ptr());
    if matrix
        != [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
    {
        return None;
    }
    let quads: Vec<usize> = if let Some((kind, indices)) = elements {
        if mode != gl::TRIANGLES
            || count <= 0
            || count % 6 != 0
            || count > 98_304
            || indices.is_null()
        {
            return None;
        }
        let indices: Vec<u32> = match kind {
            gl::UNSIGNED_SHORT => std::slice::from_raw_parts(indices.cast::<u16>(), count as usize)
                .iter()
                .map(|&i| i as u32)
                .collect(),
            gl::UNSIGNED_BYTE => std::slice::from_raw_parts(indices.cast::<u8>(), count as usize)
                .iter()
                .map(|&i| i as u32)
                .collect(),
            _ => return None,
        };
        let mut starts = Vec::with_capacity(indices.len() / 6);
        for pair in indices.chunks_exact(6) {
            let start = *pair.iter().min()?;
            let mut seen = 0;
            for &index in pair {
                let offset = index - start;
                if offset > 3 {
                    return None;
                }
                seen |= 1 << offset;
            }
            if seen != 15 || start % 4 != 0 {
                return None;
            }
            starts.push(start as usize);
        }
        starts
    } else {
        if !matches!(mode, gl::TRIANGLE_STRIP | gl::TRIANGLE_FAN)
            || count != 4
            || !(0..=65_531).contains(&first)
        {
            return None;
        }
        vec![first as usize]
    };
    let stride = integer(gles, gl::TEXTURE_COORD_ARRAY_STRIDE);
    if stride < 0 || stride != 0 && stride < 8 {
        return None;
    }
    let step = if stride == 0 { 8 } else { stride as usize };
    let mut original = std::ptr::null();
    gles.GetPointerv(gl::TEXTURE_COORD_ARRAY_POINTER, &mut original);
    if original.is_null() {
        return None;
    }
    let mut values = vec![0.0; (quads.iter().max()? + 4) * 2];
    let mut filter = 0;
    gles.GetTexParameteriv(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, &mut filter);
    let margin = sampling_margin(filter);
    let mut changed = false;
    for start in quads {
        let mut uv = [0.0; 8];
        for i in 0..4 {
            for axis in 0..2 {
                uv[i * 2 + axis] = original
                    .cast::<u8>()
                    .add((start + i) * step + axis * 4)
                    .cast::<f32>()
                    .read_unaligned();
            }
        }
        let quad_margin = if margin < 0.5 && !magnifies_quad(gles, start, &uv, dimensions) {
            0.5
        } else {
            margin
        };
        changed |= guard_quad(&mut uv, dimensions, quad_margin);
        values[start * 2..start * 2 + 8].copy_from_slice(&uv);
    }
    changed.then_some(Coordinates {
        values,
        original,
        stride,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_raster_edges_cannot_select_neighboring_images() {
        for dims in [[1024, 512], [256, 256]] {
            for mirror in [false, true] {
                let mut uv = [30.0, 41.0, 50.0, 41.0, 50.0, 42.0, 30.0, 42.0];
                if mirror {
                    for pair in uv.chunks_exact_mut(2) {
                        pair[0] = 80.0 - pair[0];
                    }
                }
                for (i, v) in uv.iter_mut().enumerate() {
                    *v /= dims[i % 2] as f32;
                }
                assert!(guard_quad(&mut uv, dims, 0.5));
                // The triangle rasterizer can cover a sample just past a true
                // edge after subpixel quantization. Test either side and mirror.
                for phase in [-0.00001, 0.0, 0.25, 0.5, 0.75, 1.0, 1.00001] {
                    let x = (uv[0] + (uv[2] - uv[0]) * phase) * dims[0] as f32;
                    let y = (uv[1] + (uv[5] - uv[1]) * phase) * dims[1] as f32;
                    assert!((30..50).contains(&(x.floor() as i32)), "x={x}");
                    assert_eq!(y.floor() as i32, 41);
                }
                // Every original native-resolution sample keeps its texel.
                for i in 0..20 {
                    let phase = (i as f32 + 0.5) / 20.0;
                    let x = (uv[0] + (uv[2] - uv[0]) * phase) * dims[0] as f32;
                    assert_eq!(x.floor() as i32, if mirror { 49 - i } else { 30 + i });
                }
            }
        }
    }

    #[test]
    fn linear_samples_during_native_scale_scroll_stay_inside_image() {
        // Native-scale magnification stays linear. A fractional camera phase
        // must not blend the neighboring packed image into a long tile edge.
        let dims = [1024, 1024];
        let mut uv = [30.0, 41.0, 50.0, 41.0, 50.0, 61.0, 30.0, 61.0];
        for value in &mut uv {
            *value /= 1024.0;
        }
        assert!(guard_quad(&mut uv, dims, 0.5));
        for i in 0..=128 {
            let phase = i as f32 / 128.0;
            for (a, b, min, max) in [(uv[0], uv[2], 30.0, 50.0), (uv[1], uv[5], 41.0, 61.0)] {
                let texel = (a + (b - a) * phase) * 1024.0;
                // Linear filtering uses adjacent texel centers. Beyond these
                // centers even a fractional contribution would leak artwork.
                assert!(texel >= min + 0.5 && texel <= max - 0.5, "{texel}");
            }
        }
    }

    #[test]
    fn minification_keeps_the_linear_footprint_guard() {
        let uv = [0.0, 0.0, 0.5, 0.0, 0.5, 0.25, 0.0, 0.25];
        for scale in [0.5, 1.0, 2.0, 3.0, 4.0] {
            let screen = [
                [0.0, 0.0],
                [512.0 * scale, 0.0],
                [512.0 * scale, 256.0 * scale],
                [0.0, 256.0 * scale],
            ];
            assert_eq!(
                magnifies_rectangle(&screen, &uv, [1024, 1024]),
                scale >= 1.0
            );
        }
        // Shearing lengthens both sides without necessarily magnifying in
        // the direction perpendicular to either edge.
        let sheared = [[0.0, 0.0], [512.0, 0.0], [1024.0, 128.0], [512.0, 128.0]];
        assert!(!magnifies_rectangle(&sheared, &uv, [1024, 1024]));
        assert!(!magnifies_rectangle(&[[0.0; 2]; 4], &uv, [1024, 1024]));
    }

    #[test]
    fn nearest_guard_preserves_uniform_pixels_at_integer_render_scales() {
        let dims = [1024, 1024];
        for width in [1, 20, 256, 480, 960] {
            for scale in 1..=4 {
                for mirror in [false, true] {
                    let mut uv = [0.0, 0.0, width as f32, 0.0, width as f32, 320.0, 0.0, 320.0];
                    if mirror {
                        for pair in uv.chunks_exact_mut(2) {
                            pair[0] = width as f32 - pair[0];
                        }
                    }
                    for v in &mut uv {
                        *v /= 1024.0;
                    }
                    assert!(guard_quad(
                        &mut uv,
                        dims,
                        sampling_margin(gl::NEAREST as GLint)
                    ));
                    for sample in 0..width * scale {
                        let phase = (sample as f32 + 0.5) / (width * scale) as f32;
                        let texel = ((uv[0] + (uv[2] - uv[0]) * phase) * 1024.0).floor() as i32;
                        let expected = if mirror {
                            width - 1 - sample / scale
                        } else {
                            sample / scale
                        };
                        assert_eq!(
                            texel, expected,
                            "width={width} scale={scale} mirror={mirror}"
                        );
                    }
                    // Edge quantization must still stay inside the source image.
                    for phase in [-0.00001, 0.0, 1.0, 1.00001] {
                        let texel = ((uv[0] + (uv[2] - uv[0]) * phase) * 1024.0).floor() as i32;
                        assert!((0..width).contains(&texel), "width={width} texel={texel}");
                    }
                }
            }
        }
        assert_eq!(sampling_margin(gl::LINEAR as GLint), 0.5);
    }

    #[test]
    fn repeating_fractional_and_nonrectangular_uvs_are_preserved() {
        for mut uv in [
            [0.0, 0.0, 2.0, 0.0, 2.0, 1.0, 0.0, 1.0],
            [0.001, 0.0, 1.0, 0.0, 1.0, 1.0, 0.001, 1.0],
            [0.0, 0.0, 1.0, 0.0, 0.5, 1.0, 0.0, 1.0],
            [0.0; 8],
        ] {
            let before = uv;
            assert!(!guard_quad(&mut uv, [256, 256], 0.5));
            assert_eq!(uv, before);
        }
    }
}
