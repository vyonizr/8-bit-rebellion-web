/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Implementation of OpenGL ES 1.1 on top of OpenGL ES 3.0, for WebGL 2.
//!
//! OpenGL ES 3.0 has no fixed-function pipeline, no client-side vertex arrays
//! (in WebGL), no fixed-point types and no buffer mapping (in WebGL). This
//! layer therefore keeps all OpenGL ES 1.1 state itself, draws with generated
//! shaders, and streams vertex data into buffers for each draw call.
//!
//! A browser canvas has a single WebGL context, so every context created here
//! shares one real context and one object namespace (like a share group).
//! Each context keeps a complete copy of its state and re-applies it when it
//! is made current.

use super::gles11_raw as gles11; // constants only
use super::gles11_raw::types::*;
use super::gles30_raw as gl3;
use super::gles_generic::GLES;
use super::util::{fixed_to_float, matrix_fixed_to_float, try_decode_pvrtc, PalettedTextureFormat};
use super::GLESContext;
use crate::window::{GLContext, GLVersion, Window};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;

/// OpenGL ES 1.1 extension constants missing from the generated bindings.
mod oes {
    use super::GLenum;
    pub const POINT_SPRITE_OES: GLenum = 0x8861;
    pub const COORD_REPLACE_OES: GLenum = 0x8862;
    pub const POINT_SIZE_ARRAY_OES: GLenum = 0x8B9C;
    pub const POINT_SIZE_ARRAY_POINTER_OES: GLenum = 0x898C;
    pub const IMPLEMENTATION_COLOR_READ_TYPE_OES: GLenum = 0x8B9A;
    pub const IMPLEMENTATION_COLOR_READ_FORMAT_OES: GLenum = 0x8B9B;
}

const TEXTURE_UNITS: usize = 2;
const LIGHTS: usize = 8;
const CLIP_PLANES: usize = 6;
const MODELVIEW_STACK_DEPTH: usize = 16;
const PROJECTION_STACK_DEPTH: usize = 2;
const TEXTURE_STACK_DEPTH: usize = 2;

const ATTRIB_POSITION: GLuint = 0;
const ATTRIB_COLOR: GLuint = 1;
const ATTRIB_NORMAL: GLuint = 2;
const ATTRIB_TEXCOORD0: GLuint = 3;

// Column-major 4x4 matrices, as OpenGL stores them.
type Mat4 = [f32; 16];
const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn mat_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut r = [0.0; 16];
    for col in 0..4 {
        for row in 0..4 {
            r[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    r
}

fn mat_transform(m: &Mat4, v: [f32; 4]) -> [f32; 4] {
    let mut r = [0.0; 4];
    for (row, out) in r.iter_mut().enumerate() {
        *out = (0..4).map(|k| m[k * 4 + row] * v[k]).sum();
    }
    r
}

/// Inverse of a 4x4 matrix, or identity if it is singular.
fn mat_inverse(m: &Mat4) -> Mat4 {
    let mut inv = [0.0f32; 16];
    inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
        + m[9] * m[7] * m[14]
        + m[13] * m[6] * m[11]
        - m[13] * m[7] * m[10];
    inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
        - m[8] * m[7] * m[14]
        - m[12] * m[6] * m[11]
        + m[12] * m[7] * m[10];
    inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
        + m[8] * m[7] * m[13]
        + m[12] * m[5] * m[11]
        - m[12] * m[7] * m[9];
    inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
        - m[8] * m[6] * m[13]
        - m[12] * m[5] * m[10]
        + m[12] * m[6] * m[9];
    inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
        - m[9] * m[3] * m[14]
        - m[13] * m[2] * m[11]
        + m[13] * m[3] * m[10];
    inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
        + m[8] * m[3] * m[14]
        + m[12] * m[2] * m[11]
        - m[12] * m[3] * m[10];
    inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
        - m[8] * m[3] * m[13]
        - m[12] * m[1] * m[11]
        + m[12] * m[3] * m[9];
    inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
        + m[8] * m[2] * m[13]
        + m[12] * m[1] * m[10]
        - m[12] * m[2] * m[9];
    inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
        + m[5] * m[3] * m[14]
        + m[13] * m[2] * m[7]
        - m[13] * m[3] * m[6];
    inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
        - m[4] * m[3] * m[14]
        - m[12] * m[2] * m[7]
        + m[12] * m[3] * m[6];
    inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
        + m[4] * m[3] * m[13]
        + m[12] * m[1] * m[7]
        - m[12] * m[3] * m[5];
    inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
        - m[4] * m[2] * m[13]
        - m[12] * m[1] * m[6]
        + m[12] * m[2] * m[5];
    inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
        - m[5] * m[3] * m[10]
        - m[9] * m[2] * m[7]
        + m[9] * m[3] * m[6];
    inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
        + m[4] * m[3] * m[10]
        + m[8] * m[2] * m[7]
        - m[8] * m[3] * m[6];
    inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
        - m[4] * m[3] * m[9]
        - m[8] * m[1] * m[7]
        + m[8] * m[3] * m[5];
    inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
        + m[4] * m[2] * m[9]
        + m[8] * m[1] * m[6]
        - m[8] * m[2] * m[5];
    let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
    if det == 0.0 {
        return IDENTITY;
    }
    inv.map(|x| x / det)
}

/// Inverse-transpose of the upper-left 3x3, for transforming normals.
fn normal_matrix(mv: &Mat4) -> [f32; 9] {
    let inv = mat_inverse(mv);
    let mut r = [0.0; 9];
    for col in 0..3 {
        for row in 0..3 {
            // transpose: element (row, col) of result = inv(col, row)
            r[col * 3 + row] = inv[row * 4 + col];
        }
    }
    r
}

fn mat_ortho(l: f32, r: f32, b: f32, t: f32, n: f32, f: f32) -> Mat4 {
    [
        2.0 / (r - l),
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 / (t - b),
        0.0,
        0.0,
        0.0,
        0.0,
        -2.0 / (f - n),
        0.0,
        -(r + l) / (r - l),
        -(t + b) / (t - b),
        -(f + n) / (f - n),
        1.0,
    ]
}

fn mat_frustum(l: f32, r: f32, b: f32, t: f32, n: f32, f: f32) -> Mat4 {
    [
        2.0 * n / (r - l),
        0.0,
        0.0,
        0.0,
        0.0,
        2.0 * n / (t - b),
        0.0,
        0.0,
        (r + l) / (r - l),
        (t + b) / (t - b),
        -(f + n) / (f - n),
        -1.0,
        0.0,
        0.0,
        -2.0 * f * n / (f - n),
        0.0,
    ]
}

fn mat_rotate(angle_deg: f32, x: f32, y: f32, z: f32) -> Mat4 {
    let len = (x * x + y * y + z * z).sqrt();
    if len == 0.0 {
        return IDENTITY;
    }
    let (x, y, z) = (x / len, y / len, z / len);
    let (s, c) = angle_deg.to_radians().sin_cos();
    let ic = 1.0 - c;
    [
        x * x * ic + c,
        y * x * ic + z * s,
        x * z * ic - y * s,
        0.0,
        x * y * ic - z * s,
        y * y * ic + c,
        y * z * ic + x * s,
        0.0,
        x * z * ic + y * s,
        y * z * ic - x * s,
        z * z * ic + c,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

/// A vertex array as set by `gl*Pointer`.
#[derive(Clone, Copy)]
struct ArrayState {
    enabled: bool,
    size: GLint,
    type_: GLenum,
    stride: GLsizei,
    /// Host pointer, or offset into `buffer` if it is non-zero.
    pointer: *const GLvoid,
    buffer: GLuint,
}
impl ArrayState {
    fn new(size: GLint) -> Self {
        ArrayState {
            enabled: false,
            size,
            type_: gles11::FLOAT,
            stride: 0,
            pointer: std::ptr::null(),
            buffer: 0,
        }
    }
}

/// The part of the texture environment that changes the generated shader.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct TexEnvKey {
    mode: GLenum,
    combine_rgb: GLenum,
    combine_alpha: GLenum,
    src_rgb: [GLenum; 3],
    src_alpha: [GLenum; 3],
    operand_rgb: [GLenum; 3],
    operand_alpha: [GLenum; 3],
}

#[derive(Clone)]
struct TexUnit {
    texture_2d: bool,
    binding: GLuint,
    env: TexEnvKey,
    env_color: [f32; 4],
    rgb_scale: f32,
    alpha_scale: f32,
    coord_replace: bool,
    matrix_stack: Vec<Mat4>,
    current_texcoord: [f32; 4],
    array: ArrayState,
}
impl TexUnit {
    fn new() -> Self {
        TexUnit {
            texture_2d: false,
            binding: 0,
            env: TexEnvKey {
                mode: gles11::MODULATE,
                combine_rgb: gles11::MODULATE,
                combine_alpha: gles11::MODULATE,
                src_rgb: [gles11::TEXTURE, gles11::PREVIOUS, gles11::CONSTANT],
                src_alpha: [gles11::TEXTURE, gles11::PREVIOUS, gles11::CONSTANT],
                operand_rgb: [gles11::SRC_COLOR, gles11::SRC_COLOR, gles11::SRC_ALPHA],
                operand_alpha: [gles11::SRC_ALPHA, gles11::SRC_ALPHA, gles11::SRC_ALPHA],
            },
            env_color: [0.0; 4],
            rgb_scale: 1.0,
            alpha_scale: 1.0,
            coord_replace: false,
            matrix_stack: vec![IDENTITY],
            current_texcoord: [0.0, 0.0, 0.0, 1.0],
            array: ArrayState::new(4),
        }
    }
}

#[derive(Clone)]
struct Light {
    ambient: [f32; 4],
    diffuse: [f32; 4],
    specular: [f32; 4],
    /// Eye-space position.
    position: [f32; 4],
    /// Eye-space direction.
    spot_direction: [f32; 3],
    spot_exponent: f32,
    spot_cutoff: f32,
    /// Constant, linear and quadratic attenuation.
    attenuation: [f32; 3],
}

#[derive(Clone)]
struct Material {
    ambient: [f32; 4],
    diffuse: [f32; 4],
    specular: [f32; 4],
    emission: [f32; 4],
    shininess: f32,
}

/// Capabilities that OpenGL ES 3.0 implements itself.
const NATIVE_CAPS: &[GLenum] = &[
    gles11::BLEND,
    gles11::CULL_FACE,
    gles11::DEPTH_TEST,
    gles11::DITHER,
    gles11::POLYGON_OFFSET_FILL,
    gles11::SAMPLE_ALPHA_TO_COVERAGE,
    gles11::SAMPLE_COVERAGE,
    gles11::SCISSOR_TEST,
    gles11::STENCIL_TEST,
];

/// Capabilities this layer emulates or accepts without effect.
const EMULATED_CAPS: &[GLenum] = &[
    gles11::ALPHA_TEST,
    gles11::COLOR_LOGIC_OP,
    gles11::COLOR_MATERIAL,
    gles11::FOG,
    gles11::LIGHTING,
    gles11::LINE_SMOOTH,
    gles11::MULTISAMPLE,
    gles11::NORMALIZE,
    gles11::POINT_SMOOTH,
    oes::POINT_SPRITE_OES,
    gles11::RESCALE_NORMAL,
    gles11::SAMPLE_ALPHA_TO_ONE,
    gles11::TEXTURE_2D,
];

/// Complete OpenGL ES 1.1 state of one context.
#[derive(Clone)]
struct State {
    // Fixed-function state this layer emulates.
    matrix_mode: GLenum,
    modelview_stack: Vec<Mat4>,
    projection_stack: Vec<Mat4>,
    units: [TexUnit; TEXTURE_UNITS],
    active_texture: usize,
    client_active_texture: usize,
    vertex_array: ArrayState,
    color_array: ArrayState,
    normal_array: ArrayState,
    point_size_array: ArrayState,
    current_color: [f32; 4],
    current_normal: [f32; 3],
    lighting: bool,
    light_enabled: [bool; LIGHTS],
    lights: [Light; LIGHTS],
    light_model_ambient: [f32; 4],
    light_model_two_side: bool,
    material: Material,
    color_material: bool,
    normalize: bool,
    rescale_normal: bool,
    fog: bool,
    fog_mode: GLenum,
    fog_density: f32,
    fog_start: f32,
    fog_end: f32,
    fog_color: [f32; 4],
    alpha_test: bool,
    alpha_func: GLenum,
    alpha_ref: f32,
    shade_model: GLenum,
    clip_plane_enabled: [bool; CLIP_PLANES],
    /// Eye-space plane equations.
    clip_planes: [[f32; 4]; CLIP_PLANES],
    point_size: f32,
    point_sprite: bool,
    point_size_min: f32,
    point_size_max: f32,
    point_fade_threshold: f32,
    point_attenuation: [f32; 3],
    other_caps: HashMap<GLenum, bool>,
    hints: HashMap<GLenum, GLenum>,
    logic_op: GLenum,
    line_width: f32,
    error: GLenum,

    // State OpenGL ES 3.0 implements, mirrored so it can be re-applied when
    // this context becomes current again.
    native_caps: HashMap<GLenum, bool>,
    blend_func: [GLenum; 2],
    blend_equation: GLenum,
    color_mask: [GLboolean; 4],
    depth_func: GLenum,
    depth_mask: GLboolean,
    depth_range: [f32; 2],
    cull_face_mode: GLenum,
    front_face: GLenum,
    polygon_offset: [f32; 2],
    sample_coverage: (f32, GLboolean),
    scissor: [GLint; 4],
    viewport: [GLint; 4],
    stencil_func: (GLenum, GLint, GLuint),
    stencil_op: [GLenum; 3],
    stencil_writemask: GLuint,
    clear_color: [f32; 4],
    clear_depth: f32,
    clear_stencil: GLint,
    pack_alignment: GLint,
    unpack_alignment: GLint,
    array_buffer: GLuint,
    element_array_buffer: GLuint,
    framebuffer: GLuint,
    renderbuffer: GLuint,
}

impl State {
    fn new() -> Self {
        let light = |i: usize| Light {
            ambient: [0.0, 0.0, 0.0, 1.0],
            diffuse: if i == 0 { [1.0; 4] } else { [0.0, 0.0, 0.0, 1.0] },
            specular: if i == 0 { [1.0; 4] } else { [0.0, 0.0, 0.0, 1.0] },
            position: [0.0, 0.0, 1.0, 0.0],
            spot_direction: [0.0, 0.0, -1.0],
            spot_exponent: 0.0,
            spot_cutoff: 180.0,
            attenuation: [1.0, 0.0, 0.0],
        };
        State {
            matrix_mode: gles11::MODELVIEW,
            modelview_stack: vec![IDENTITY],
            projection_stack: vec![IDENTITY],
            units: [TexUnit::new(), TexUnit::new()],
            active_texture: 0,
            client_active_texture: 0,
            vertex_array: ArrayState::new(4),
            color_array: ArrayState::new(4),
            normal_array: ArrayState::new(3),
            point_size_array: ArrayState::new(1),
            current_color: [1.0; 4],
            current_normal: [0.0, 0.0, 1.0],
            lighting: false,
            light_enabled: [false; LIGHTS],
            lights: std::array::from_fn(light),
            light_model_ambient: [0.2, 0.2, 0.2, 1.0],
            light_model_two_side: false,
            material: Material {
                ambient: [0.2, 0.2, 0.2, 1.0],
                diffuse: [0.8, 0.8, 0.8, 1.0],
                specular: [0.0, 0.0, 0.0, 1.0],
                emission: [0.0, 0.0, 0.0, 1.0],
                shininess: 0.0,
            },
            color_material: false,
            normalize: false,
            rescale_normal: false,
            fog: false,
            fog_mode: gles11::EXP,
            fog_density: 1.0,
            fog_start: 0.0,
            fog_end: 1.0,
            fog_color: [0.0; 4],
            alpha_test: false,
            alpha_func: gles11::ALWAYS,
            alpha_ref: 0.0,
            shade_model: gles11::SMOOTH,
            clip_plane_enabled: [false; CLIP_PLANES],
            clip_planes: [[0.0; 4]; CLIP_PLANES],
            point_size: 1.0,
            point_sprite: false,
            point_size_min: 0.0,
            point_size_max: 64.0,
            point_fade_threshold: 1.0,
            point_attenuation: [1.0, 0.0, 0.0],
            other_caps: HashMap::new(),
            hints: HashMap::new(),
            logic_op: gles11::COPY,
            line_width: 1.0,
            error: gles11::NO_ERROR,

            native_caps: NATIVE_CAPS
                .iter()
                .map(|&cap| (cap, cap == gles11::DITHER))
                .collect(),
            blend_func: [gles11::ONE, gles11::ZERO],
            blend_equation: gles11::FUNC_ADD_OES,
            color_mask: [gles11::TRUE; 4],
            depth_func: gles11::LESS,
            depth_mask: gles11::TRUE,
            depth_range: [0.0, 1.0],
            cull_face_mode: gles11::BACK,
            front_face: gles11::CCW,
            polygon_offset: [0.0, 0.0],
            sample_coverage: (1.0, gles11::FALSE),
            scissor: [0, 0, 0, 0],
            viewport: [0, 0, 0, 0],
            stencil_func: (gles11::ALWAYS, 0, !0),
            stencil_op: [gles11::KEEP; 3],
            stencil_writemask: !0,
            clear_color: [0.0; 4],
            clear_depth: 1.0,
            clear_stencil: 0,
            pack_alignment: 4,
            unpack_alignment: 4,
            array_buffer: 0,
            element_array_buffer: 0,
            framebuffer: 0,
            renderbuffer: 0,
        }
    }

    fn current_matrix_stack(&mut self) -> &mut Vec<Mat4> {
        match self.matrix_mode {
            gles11::MODELVIEW => &mut self.modelview_stack,
            gles11::PROJECTION => &mut self.projection_stack,
            gles11::TEXTURE => &mut self.units[self.active_texture].matrix_stack,
            _ => unreachable!(),
        }
    }
    fn current_matrix(&mut self) -> &mut Mat4 {
        self.current_matrix_stack().last_mut().unwrap()
    }
    fn modelview(&self) -> &Mat4 {
        self.modelview_stack.last().unwrap()
    }

    fn set_error(&mut self, error: GLenum) {
        if self.error == gles11::NO_ERROR {
            self.error = error;
        }
    }

    fn array_mut(&mut self, name: GLenum) -> Option<&mut ArrayState> {
        Some(match name {
            gles11::VERTEX_ARRAY => &mut self.vertex_array,
            gles11::COLOR_ARRAY => &mut self.color_array,
            gles11::NORMAL_ARRAY => &mut self.normal_array,
            gles11::TEXTURE_COORD_ARRAY => &mut self.units[self.client_active_texture].array,
            oes::POINT_SIZE_ARRAY_OES => &mut self.point_size_array,
            _ => return None,
        })
    }

    /// Push every piece of mirrored native state to the real context.
    unsafe fn apply_all(&self) {
        for (&cap, &on) in &self.native_caps {
            if on {
                gl3::Enable(cap)
            } else {
                gl3::Disable(cap)
            }
        }
        gl3::BlendFunc(self.blend_func[0], self.blend_func[1]);
        gl3::BlendEquation(self.blend_equation);
        let [r, g, b, a] = self.color_mask;
        gl3::ColorMask(r, g, b, a);
        gl3::DepthFunc(self.depth_func);
        gl3::DepthMask(self.depth_mask);
        gl3::DepthRangef(self.depth_range[0], self.depth_range[1]);
        gl3::CullFace(self.cull_face_mode);
        gl3::FrontFace(self.front_face);
        gl3::PolygonOffset(self.polygon_offset[0], self.polygon_offset[1]);
        gl3::SampleCoverage(self.sample_coverage.0, self.sample_coverage.1);
        let [x, y, w, h] = self.scissor;
        gl3::Scissor(x, y, w, h);
        let [x, y, w, h] = self.viewport;
        gl3::Viewport(x, y, w, h);
        let (func, ref_, mask) = self.stencil_func;
        gl3::StencilFunc(func, ref_, mask);
        gl3::StencilOp(self.stencil_op[0], self.stencil_op[1], self.stencil_op[2]);
        gl3::StencilMask(self.stencil_writemask);
        let [r, g, b, a] = self.clear_color;
        gl3::ClearColor(r, g, b, a);
        gl3::ClearDepthf(self.clear_depth);
        gl3::ClearStencil(self.clear_stencil);
        gl3::PixelStorei(gl3::PACK_ALIGNMENT, self.pack_alignment);
        gl3::PixelStorei(gl3::UNPACK_ALIGNMENT, self.unpack_alignment);
        for (i, unit) in self.units.iter().enumerate() {
            gl3::ActiveTexture(gl3::TEXTURE0 + i as GLenum);
            gl3::BindTexture(gl3::TEXTURE_2D, unit.binding);
        }
        gl3::ActiveTexture(gl3::TEXTURE0 + self.active_texture as GLenum);
        gl3::BindBuffer(gl3::ARRAY_BUFFER, self.array_buffer);
        gl3::BindBuffer(gl3::ELEMENT_ARRAY_BUFFER, self.element_array_buffer);
        gl3::BindFramebuffer(gl3::FRAMEBUFFER, self.framebuffer);
        gl3::BindRenderbuffer(gl3::RENDERBUFFER, self.renderbuffer);
    }
}

/// Everything in a generated shader that depends on state.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct ShaderKey {
    flat: bool,
    lighting: bool,
    lights: u8,
    color_material: bool,
    normalize: bool,
    /// Per texture unit: enabled, texture base format, environment.
    units: [Option<(GLenum, TexEnvKey, bool)>; TEXTURE_UNITS],
    alpha_func: Option<GLenum>,
    fog_mode: Option<GLenum>,
    clip_planes: u8,
}

struct Program {
    program: GLuint,
    uniforms: HashMap<&'static str, GLint>,
}

/// Objects shared by all contexts, because they share one real context.
struct Shared {
    loaded: bool,
    /// The context whose state is currently applied to the real context.
    current_context: u64,
    next_context_id: u64,
    programs: HashMap<ShaderKey, Program>,
    /// CPU copies of buffer contents, needed for mapping and for converting
    /// vertex data WebGL can't use directly.
    buffer_data: HashMap<GLuint, Vec<u8>>,
    /// Base format of each texture's level 0, which affects the texture
    /// environment's behavior.
    texture_formats: HashMap<GLuint, GLenum>,
    /// Textures with `GL_GENERATE_MIPMAP` set.
    generate_mipmap: HashMap<GLuint, bool>,
    stream_vertex_buffer: GLuint,
    stream_index_buffer: GLuint,
    staging: Vec<u8>,
    index_staging: Vec<u8>,
    /// Pending glMapBufferOES: (buffer, target).
    mapped: Option<GLuint>,
}

thread_local! {
    static SHARED: RefCell<Shared> = RefCell::new(Shared {
        loaded: false,
        current_context: 0,
        next_context_id: 1,
        programs: HashMap::new(),
        buffer_data: HashMap::new(),
        texture_formats: HashMap::new(),
        generate_mipmap: HashMap::new(),
        stream_vertex_buffer: 0,
        stream_index_buffer: 0,
        staging: Vec::new(),
        index_staging: Vec::new(),
        mapped: None,
    });
}

fn with_shared<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    SHARED.with(|s| f(&mut s.borrow_mut()))
}

pub struct GLES1OnGLES3Context {
    gl_ctx: GLContext,
    id: u64,
    state: State,
}

impl GLES1OnGLES3Context {
    /// Make the real context current, load functions if needed and re-apply
    /// this context's state if another context was current.
    unsafe fn activate(&mut self, load: &mut dyn FnMut(&'static str) -> *const std::ffi::c_void) {
        let (loaded, current) = with_shared(|s| (s.loaded, s.current_context));
        if !loaded {
            gl3::load_with(|s| load(s));
            let (mut vb, mut ib) = (0, 0);
            gl3::GenBuffers(1, &mut vb);
            gl3::GenBuffers(1, &mut ib);
            with_shared(|s| {
                s.loaded = true;
                s.stream_vertex_buffer = vb;
                s.stream_index_buffer = ib;
            });
        }
        if current != self.id {
            self.state.apply_all();
            with_shared(|s| s.current_context = self.id);
        }
    }
}

impl GLESContext for GLES1OnGLES3Context {
    fn description() -> &'static str {
        "OpenGL ES 1.1 via touchHLE GLES1-on-GLES3 layer"
    }

    fn new(window: &mut Window) -> Result<Self, String> {
        let gl_ctx = window.create_gl_context(GLVersion::GLES30)?;
        let id = with_shared(|s| {
            s.next_context_id += 1;
            s.next_context_id
        });
        Ok(Self {
            gl_ctx,
            id,
            state: State::new(),
        })
    }

    fn make_current<'gl_ctx, 'win: 'gl_ctx>(
        &'gl_ctx mut self,
        window: &'win mut Window,
    ) -> Box<dyn GLES + 'gl_ctx> {
        unsafe {
            window.make_gl_context_current(&self.gl_ctx);
            self.activate(&mut |s| window.gl_get_proc_address(s));
        }
        Box::new(GLES1OnGLES3 {
            state: &mut self.state,
        })
    }

    unsafe fn make_current_unchecked_for_window<'gl_ctx>(
        &'gl_ctx mut self,
        make_current_fn: &mut dyn FnMut(&GLContext),
        loader_fn: &mut dyn FnMut(&'static str) -> *const std::ffi::c_void,
    ) -> Box<dyn GLES + 'gl_ctx> {
        make_current_fn(&self.gl_ctx);
        self.activate(loader_fn);
        Box::new(GLES1OnGLES3 {
            state: &mut self.state,
        })
    }
}

pub struct GLES1OnGLES3<'a> {
    state: &'a mut State,
}

fn type_size(type_: GLenum) -> usize {
    match type_ {
        gles11::BYTE | gles11::UNSIGNED_BYTE => 1,
        gles11::SHORT | gles11::UNSIGNED_SHORT => 2,
        gles11::FLOAT | gles11::FIXED => 4,
        _ => panic!("Unexpected vertex attribute type {type_:#x}"),
    }
}

/// Read one vertex attribute component as a float, applying OpenGL's
/// normalization rules for `normalized` integer types.
unsafe fn read_component(ptr: *const u8, type_: GLenum, normalized: bool) -> f32 {
    match type_ {
        gles11::FLOAT => ptr.cast::<f32>().read_unaligned(),
        gles11::FIXED => fixed_to_float(ptr.cast::<GLfixed>().read_unaligned()),
        gles11::BYTE => {
            let v = ptr.cast::<i8>().read() as f32;
            if normalized {
                (2.0 * v + 1.0) / 255.0
            } else {
                v
            }
        }
        gles11::UNSIGNED_BYTE => {
            let v = ptr.read() as f32;
            if normalized {
                v / 255.0
            } else {
                v
            }
        }
        gles11::SHORT => {
            let v = ptr.cast::<i16>().read_unaligned() as f32;
            if normalized {
                (2.0 * v + 1.0) / 65535.0
            } else {
                v
            }
        }
        gles11::UNSIGNED_SHORT => {
            let v = ptr.cast::<u16>().read_unaligned() as f32;
            if normalized {
                v / 65535.0
            } else {
                v
            }
        }
        _ => unreachable!(),
    }
}

fn glsl_compare(func: GLenum) -> &'static str {
    match func {
        gles11::LESS => "<",
        gles11::EQUAL => "==",
        gles11::LEQUAL => "<=",
        gles11::GREATER => ">",
        gles11::NOTEQUAL => "!=",
        gles11::GEQUAL => ">=",
        _ => unreachable!(),
    }
}

/// Texture base formats with color (rather than only alpha) components.
fn format_has_color(format: GLenum) -> bool {
    format != gles11::ALPHA
}
fn format_has_alpha(format: GLenum) -> bool {
    matches!(
        format,
        gles11::ALPHA | gles11::LUMINANCE_ALPHA | gles11::RGBA
    )
}

/// GLSL for one texture unit's environment. `prev` is the incoming color, `t`
/// the texel, `env` the environment color and `primary` the vertex color.
fn texenv_glsl(i: usize, format: GLenum, env: &TexEnvKey) -> String {
    let hc = format_has_color(format);
    let ha = format_has_alpha(format);
    let (rgb, a) = match env.mode {
        gles11::REPLACE => (
            if hc { "t.rgb" } else { "prev.rgb" }.to_string(),
            if ha { "t.a" } else { "prev.a" }.to_string(),
        ),
        gles11::MODULATE => (
            if hc { "prev.rgb * t.rgb" } else { "prev.rgb" }.to_string(),
            if ha { "prev.a * t.a" } else { "prev.a" }.to_string(),
        ),
        gles11::DECAL => (
            match format {
                gles11::RGB => "t.rgb",
                gles11::RGBA => "mix(prev.rgb, t.rgb, t.a)",
                _ => "prev.rgb",
            }
            .to_string(),
            "prev.a".to_string(),
        ),
        gles11::BLEND => (
            if hc {
                format!("mix(prev.rgb, u_env_color[{i}].rgb, t.rgb)")
            } else {
                "prev.rgb".to_string()
            },
            if ha { "prev.a * t.a" } else { "prev.a" }.to_string(),
        ),
        gles11::ADD => (
            if hc { "prev.rgb + t.rgb" } else { "prev.rgb" }.to_string(),
            if ha { "prev.a * t.a" } else { "prev.a" }.to_string(),
        ),
        gles11::COMBINE => {
            let src = |s: GLenum| match s {
                gles11::TEXTURE => "t".to_string(),
                gles11::CONSTANT => format!("u_env_color[{i}]"),
                gles11::PRIMARY_COLOR => "primary".to_string(),
                gles11::PREVIOUS => "prev".to_string(),
                _ => panic!("Unexpected texture combiner source {s:#x}"),
            };
            let arg_rgb = |n: usize| {
                let s = src(env.src_rgb[n]);
                match env.operand_rgb[n] {
                    gles11::SRC_COLOR => format!("{s}.rgb"),
                    gles11::ONE_MINUS_SRC_COLOR => format!("(1.0 - {s}.rgb)"),
                    gles11::SRC_ALPHA => format!("vec3({s}.a)"),
                    gles11::ONE_MINUS_SRC_ALPHA => format!("vec3(1.0 - {s}.a)"),
                    op => panic!("Unexpected texture combiner operand {op:#x}"),
                }
            };
            let arg_a = |n: usize| {
                let s = src(env.src_alpha[n]);
                match env.operand_alpha[n] {
                    gles11::SRC_ALPHA => format!("{s}.a"),
                    gles11::ONE_MINUS_SRC_ALPHA => format!("(1.0 - {s}.a)"),
                    op => panic!("Unexpected texture combiner operand {op:#x}"),
                }
            };
            let func = |f: GLenum, arg: &dyn Fn(usize) -> String, half: &str| match f {
                gles11::REPLACE => arg(0),
                gles11::MODULATE => format!("{} * {}", arg(0), arg(1)),
                gles11::ADD => format!("{} + {}", arg(0), arg(1)),
                gles11::ADD_SIGNED => format!("{} + {} - {half}", arg(0), arg(1)),
                gles11::INTERPOLATE => format!(
                    "{} * {} + {} * (1.0 - {})",
                    arg(0),
                    arg(2),
                    arg(1),
                    arg(2)
                ),
                gles11::SUBTRACT => format!("{} - {}", arg(0), arg(1)),
                _ => panic!("Unexpected texture combiner function {f:#x}"),
            };
            let dot3 = || {
                format!(
                    "4.0 * dot({} - vec3(0.5), {} - vec3(0.5))",
                    arg_rgb(0),
                    arg_rgb(1)
                )
            };
            let rgb = match env.combine_rgb {
                gles11::DOT3_RGB | gles11::DOT3_RGBA => format!("vec3({})", dot3()),
                f => func(f, &arg_rgb, "vec3(0.5)"),
            };
            let a = if env.combine_rgb == gles11::DOT3_RGBA {
                dot3()
            } else {
                func(env.combine_alpha, &arg_a, "0.5")
            };
            (
                format!("({rgb}) * u_env_scale[{i}].x"),
                format!("({a}) * u_env_scale[{i}].y"),
            )
        }
        mode => panic!("Unexpected texture environment mode {mode:#x}"),
    };
    format!("prev = clamp(vec4({rgb}, {a}), 0.0, 1.0);\n")
}

fn generate_shaders(key: &ShaderKey) -> (String, String) {
    let flat = if key.flat { "flat " } else { "" };
    let mut vs = String::new();
    vs.push_str(
        "#version 300 es\n\
         precision highp float;\n\
         layout(location = 0) in vec4 a_position;\n\
         layout(location = 1) in vec4 a_color;\n\
         layout(location = 2) in vec3 a_normal;\n\
         layout(location = 3) in vec4 a_texcoord0;\n\
         layout(location = 4) in vec4 a_texcoord1;\n\
         uniform mat4 u_modelview;\n\
         uniform mat4 u_projection;\n\
         uniform mat4 u_texture_matrix[2];\n\
         uniform float u_point_size;\n\
         uniform mat3 u_normal_matrix;\n\
         uniform vec4 u_light_model_ambient;\n\
         uniform vec4 u_material_ambient;\n\
         uniform vec4 u_material_diffuse;\n\
         uniform vec4 u_material_specular;\n\
         uniform vec4 u_material_emission;\n\
         uniform float u_material_shininess;\n\
         uniform vec4 u_light_ambient[8];\n\
         uniform vec4 u_light_diffuse[8];\n\
         uniform vec4 u_light_specular[8];\n\
         uniform vec4 u_light_position[8];\n\
         uniform vec3 u_light_spot_direction[8];\n\
         uniform float u_light_spot_exponent[8];\n\
         uniform float u_light_spot_cutoff[8];\n\
         uniform vec3 u_light_attenuation[8];\n",
    );
    vs.push_str(&format!(
        "{flat}out vec4 v_color;\nout vec4 v_texcoord0;\nout vec4 v_texcoord1;\nout vec4 v_eye;\n"
    ));
    vs.push_str(
        "void main() {\n\
         vec4 eye = u_modelview * a_position;\n\
         v_eye = eye;\n\
         gl_Position = u_projection * eye;\n\
         gl_PointSize = u_point_size;\n\
         v_texcoord0 = u_texture_matrix[0] * a_texcoord0;\n\
         v_texcoord1 = u_texture_matrix[1] * a_texcoord1;\n",
    );
    if key.lighting {
        vs.push_str("vec3 n = u_normal_matrix * a_normal;\n");
        if key.normalize {
            vs.push_str("n = normalize(n);\n");
        }
        if key.color_material {
            vs.push_str("vec4 amb = a_color;\nvec4 dif = a_color;\n");
        } else {
            vs.push_str("vec4 amb = u_material_ambient;\nvec4 dif = u_material_diffuse;\n");
        }
        vs.push_str("vec3 c = u_material_emission.rgb + u_light_model_ambient.rgb * amb.rgb;\n");
        for i in 0..LIGHTS {
            if key.lights & (1 << i) == 0 {
                continue;
            }
            vs.push_str(&format!(
                "{{\n\
                 vec4 p = u_light_position[{i}];\n\
                 vec3 l;\n\
                 float att = 1.0;\n\
                 if (p.w != 0.0) {{\n\
                   vec3 d = p.xyz / p.w - eye.xyz / eye.w;\n\
                   float dist = length(d);\n\
                   l = d / dist;\n\
                   vec3 k = u_light_attenuation[{i}];\n\
                   att = 1.0 / (k.x + k.y * dist + k.z * dist * dist);\n\
                 }} else {{\n\
                   l = normalize(p.xyz);\n\
                 }}\n\
                 if (u_light_spot_cutoff[{i}] != 180.0) {{\n\
                   float sd = dot(-l, normalize(u_light_spot_direction[{i}]));\n\
                   att *= sd >= cos(radians(u_light_spot_cutoff[{i}]))\n\
                     ? pow(max(sd, 0.0), u_light_spot_exponent[{i}]) : 0.0;\n\
                 }}\n\
                 float ndotl = max(dot(n, l), 0.0);\n\
                 vec3 h = normalize(l + vec3(0.0, 0.0, 1.0));\n\
                 float spec = ndotl > 0.0\n\
                   ? (u_material_shininess == 0.0 ? 1.0 : pow(max(dot(n, h), 0.0), u_material_shininess))\n\
                   : 0.0;\n\
                 c += att * (u_light_ambient[{i}].rgb * amb.rgb\n\
                   + ndotl * u_light_diffuse[{i}].rgb * dif.rgb\n\
                   + spec * u_light_specular[{i}].rgb * u_material_specular.rgb);\n\
                 }}\n"
            ));
        }
        vs.push_str("v_color = vec4(clamp(c, 0.0, 1.0), clamp(dif.a, 0.0, 1.0));\n");
    } else {
        vs.push_str("v_color = a_color;\n");
    }
    vs.push_str("}\n");

    let mut fs = String::new();
    fs.push_str(
        "#version 300 es\n\
         precision highp float;\n\
         uniform sampler2D u_texture[2];\n\
         uniform vec4 u_env_color[2];\n\
         uniform vec2 u_env_scale[2];\n\
         uniform float u_alpha_ref;\n\
         uniform vec4 u_fog_color;\n\
         uniform vec3 u_fog_params;\n\
         uniform vec4 u_clip_plane[6];\n",
    );
    fs.push_str(&format!(
        "{flat}in vec4 v_color;\nin vec4 v_texcoord0;\nin vec4 v_texcoord1;\nin vec4 v_eye;\nout vec4 o_color;\n"
    ));
    fs.push_str("void main() {\n");
    for i in 0..CLIP_PLANES {
        if key.clip_planes & (1 << i) != 0 {
            fs.push_str(&format!(
                "if (dot(u_clip_plane[{i}], v_eye) < 0.0) discard;\n"
            ));
        }
    }
    fs.push_str("vec4 primary = v_color;\nvec4 prev = primary;\nvec4 t;\n");
    for (i, unit) in key.units.iter().enumerate() {
        let Some((format, env, coord_replace)) = unit else {
            continue;
        };
        if *coord_replace {
            fs.push_str(&format!("t = texture(u_texture[{i}], gl_PointCoord);\n"));
        } else {
            fs.push_str(&format!(
                "t = textureProj(u_texture[{i}], v_texcoord{i});\n"
            ));
        }
        fs.push_str(&texenv_glsl(i, *format, env));
    }
    if let Some(mode) = key.fog_mode {
        fs.push_str("float z = abs(v_eye.z);\nfloat f;\n");
        fs.push_str(match mode {
            gles11::LINEAR => "f = (u_fog_params.y - z) / (u_fog_params.y - u_fog_params.x);\n",
            gles11::EXP => "f = exp(-u_fog_params.z * z);\n",
            gles11::EXP2 => "f = exp(-(u_fog_params.z * z) * (u_fog_params.z * z));\n",
            _ => unreachable!(),
        });
        fs.push_str("prev.rgb = mix(u_fog_color.rgb, prev.rgb, clamp(f, 0.0, 1.0));\n");
    }
    match key.alpha_func {
        None | Some(gles11::ALWAYS) => (),
        Some(gles11::NEVER) => fs.push_str("discard;\n"),
        Some(func) => fs.push_str(&format!(
            "if (!(prev.a {} u_alpha_ref)) discard;\n",
            glsl_compare(func)
        )),
    }
    fs.push_str("o_color = prev;\n}\n");
    (vs, fs)
}

unsafe fn compile_shader(kind: GLenum, source: &str) -> GLuint {
    let shader = gl3::CreateShader(kind);
    let source = CString::new(source).unwrap();
    gl3::ShaderSource(shader, 1, &source.as_ptr(), std::ptr::null());
    gl3::CompileShader(shader);
    let mut ok = 0;
    gl3::GetShaderiv(shader, gl3::COMPILE_STATUS, &mut ok);
    if ok == 0 {
        let mut log = vec![0u8; 4096];
        let mut len = 0;
        gl3::GetShaderInfoLog(shader, log.len() as _, &mut len, log.as_mut_ptr().cast());
        panic!(
            "Shader compilation failed: {}\n{}",
            String::from_utf8_lossy(&log[..len as usize]),
            source.to_string_lossy()
        );
    }
    shader
}

const UNIFORM_NAMES: &[&str] = &[
    "u_modelview",
    "u_projection",
    "u_texture_matrix[0]",
    "u_texture_matrix[1]",
    "u_point_size",
    "u_normal_matrix",
    "u_light_model_ambient",
    "u_material_ambient",
    "u_material_diffuse",
    "u_material_specular",
    "u_material_emission",
    "u_material_shininess",
    "u_texture[0]",
    "u_texture[1]",
    "u_env_color[0]",
    "u_env_color[1]",
    "u_env_scale[0]",
    "u_env_scale[1]",
    "u_alpha_ref",
    "u_fog_color",
    "u_fog_params",
];

/// Names of per-light and per-plane uniforms, built once.
fn indexed_uniform_names() -> &'static [String] {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names = Vec::new();
        for field in [
            "ambient",
            "diffuse",
            "specular",
            "position",
            "spot_direction",
            "spot_exponent",
            "spot_cutoff",
            "attenuation",
        ] {
            for i in 0..LIGHTS {
                names.push(format!("u_light_{field}[{i}]"));
            }
        }
        for i in 0..CLIP_PLANES {
            names.push(format!("u_clip_plane[{i}]"));
        }
        names
    })
}

unsafe fn build_program(key: &ShaderKey) -> Program {
    let (vs_src, fs_src) = generate_shaders(key);
    let vs = compile_shader(gl3::VERTEX_SHADER, &vs_src);
    let fs = compile_shader(gl3::FRAGMENT_SHADER, &fs_src);
    let program = gl3::CreateProgram();
    gl3::AttachShader(program, vs);
    gl3::AttachShader(program, fs);
    gl3::LinkProgram(program);
    let mut ok = 0;
    gl3::GetProgramiv(program, gl3::LINK_STATUS, &mut ok);
    assert!(ok != 0, "Shader program failed to link");
    gl3::DeleteShader(vs);
    gl3::DeleteShader(fs);
    let mut uniforms = HashMap::new();
    let names = UNIFORM_NAMES
        .iter()
        .copied()
        .chain(indexed_uniform_names().iter().map(|s| s.as_str()));
    for name in names {
        let c_name = CString::new(name).unwrap();
        let loc = gl3::GetUniformLocation(program, c_name.as_ptr());
        if loc >= 0 {
            uniforms.insert(name, loc);
        }
    }
    Program { program, uniforms }
}

/// A vertex attribute prepared for a draw call.
enum AttribSource {
    /// Use a real buffer object directly.
    Buffer {
        buffer: GLuint,
        size: GLint,
        type_: GLenum,
        normalized: bool,
        stride: GLsizei,
        offset: usize,
    },
    /// Data copied into the streaming buffer at this offset.
    Stream {
        size: GLint,
        type_: GLenum,
        normalized: bool,
        stride: GLsizei,
        offset: usize,
    },
    /// No array: a constant value.
    Constant([f32; 4]),
}

impl GLES1OnGLES3<'_> {
    fn texture_format(&self, unit: usize) -> GLenum {
        let binding = self.state.units[unit].binding;
        with_shared(|s| s.texture_formats.get(&binding).copied()).unwrap_or(gles11::RGBA)
    }

    fn shader_key(&self) -> ShaderKey {
        let s = &self.state;
        let mut lights = 0u8;
        for (i, &on) in s.light_enabled.iter().enumerate() {
            if on {
                lights |= 1 << i;
            }
        }
        let mut clip_planes = 0u8;
        for (i, &on) in s.clip_plane_enabled.iter().enumerate() {
            if on {
                clip_planes |= 1 << i;
            }
        }
        let units = std::array::from_fn(|i| {
            let unit = &s.units[i];
            unit.texture_2d.then(|| {
                (
                    self.texture_format(i),
                    unit.env,
                    s.point_sprite && unit.coord_replace,
                )
            })
        });
        ShaderKey {
            flat: s.shade_model == gles11::FLAT,
            lighting: s.lighting,
            lights: if s.lighting { lights } else { 0 },
            color_material: s.lighting && s.color_material,
            normalize: s.lighting && (s.normalize || s.rescale_normal),
            units,
            alpha_func: s.alpha_test.then_some(s.alpha_func),
            fog_mode: s.fog.then_some(s.fog_mode),
            clip_planes,
        }
    }

    unsafe fn set_uniforms(&self, program: &Program) {
        let s = &self.state;
        let u = |name: &str| program.uniforms.get(name).copied();
        let mat4 = |name: &str, m: &Mat4| {
            if let Some(loc) = u(name) {
                gl3::UniformMatrix4fv(loc, 1, gl3::FALSE, m.as_ptr());
            }
        };
        let vec4 = |name: &str, v: &[f32; 4]| {
            if let Some(loc) = u(name) {
                gl3::Uniform4fv(loc, 1, v.as_ptr());
            }
        };
        let vec3 = |name: &str, v: &[f32; 3]| {
            if let Some(loc) = u(name) {
                gl3::Uniform3fv(loc, 1, v.as_ptr());
            }
        };
        let float = |name: &str, v: f32| {
            if let Some(loc) = u(name) {
                gl3::Uniform1f(loc, v);
            }
        };
        let mv = s.modelview();
        mat4("u_modelview", mv);
        mat4("u_projection", s.projection_stack.last().unwrap());
        mat4("u_texture_matrix[0]", s.units[0].matrix_stack.last().unwrap());
        mat4("u_texture_matrix[1]", s.units[1].matrix_stack.last().unwrap());
        float(
            "u_point_size",
            s.point_size.clamp(s.point_size_min.max(1.0), s.point_size_max),
        );
        if let Some(loc) = u("u_texture[0]") {
            gl3::Uniform1i(loc, 0);
        }
        if let Some(loc) = u("u_texture[1]") {
            gl3::Uniform1i(loc, 1);
        }
        for i in 0..TEXTURE_UNITS {
            vec4(&format!("u_env_color[{i}]"), &s.units[i].env_color);
            if let Some(loc) = u(&format!("u_env_scale[{i}]")) {
                gl3::Uniform2f(loc, s.units[i].rgb_scale, s.units[i].alpha_scale);
            }
        }
        float("u_alpha_ref", s.alpha_ref);
        vec4("u_fog_color", &s.fog_color);
        vec3("u_fog_params", &[s.fog_start, s.fog_end, s.fog_density]);
        let names = indexed_uniform_names();
        for i in 0..CLIP_PLANES {
            if s.clip_plane_enabled[i] {
                vec4(&names[LIGHTS * 8 + i], &s.clip_planes[i]);
            }
        }
        if s.lighting {
            if let Some(loc) = u("u_normal_matrix") {
                gl3::UniformMatrix3fv(loc, 1, gl3::FALSE, normal_matrix(mv).as_ptr());
            }
            vec4("u_light_model_ambient", &s.light_model_ambient);
            let m = &s.material;
            vec4("u_material_ambient", &m.ambient);
            vec4("u_material_diffuse", &m.diffuse);
            vec4("u_material_specular", &m.specular);
            vec4("u_material_emission", &m.emission);
            float("u_material_shininess", m.shininess);
            for (i, light) in s.lights.iter().enumerate() {
                if !s.light_enabled[i] {
                    continue;
                }
                vec4(&names[i], &light.ambient);
                vec4(&names[LIGHTS + i], &light.diffuse);
                vec4(&names[LIGHTS * 2 + i], &light.specular);
                vec4(&names[LIGHTS * 3 + i], &light.position);
                vec3(&names[LIGHTS * 4 + i], &light.spot_direction);
                float(&names[LIGHTS * 5 + i], light.spot_exponent);
                float(&names[LIGHTS * 6 + i], light.spot_cutoff);
                vec3(&names[LIGHTS * 7 + i], &light.attenuation);
            }
        }
    }

    /// Prepare one vertex attribute for vertices `min..=max`, rebased so
    /// vertex `min` becomes vertex 0.
    unsafe fn prepare_attrib(
        array: &ArrayState,
        normalized: bool,
        min: usize,
        max: usize,
        staging: &mut Vec<u8>,
        shared_buffers: &HashMap<GLuint, Vec<u8>>,
    ) -> AttribSource {
        let elem_size = type_size(array.type_) * array.size as usize;
        let stride = if array.stride == 0 {
            elem_size
        } else {
            array.stride as usize
        };
        let tsize = type_size(array.type_);
        let aligned = (array.pointer as usize) % tsize == 0 && stride % tsize == 0;
        if array.buffer != 0 && array.type_ != gles11::FIXED && aligned {
            return AttribSource::Buffer {
                buffer: array.buffer,
                size: array.size,
                type_: array.type_,
                normalized,
                stride: stride as GLsizei,
                offset: array.pointer as usize + min * stride,
            };
        }
        let base: *const u8 = if array.buffer != 0 {
            let data = shared_buffers
                .get(&array.buffer)
                .expect("Vertex array uses an unknown buffer");
            assert!(array.pointer as usize + max * stride + elem_size <= data.len());
            data.as_ptr().add(array.pointer as usize)
        } else {
            array.pointer.cast()
        };
        // Align each attribute's data to 4 bytes.
        while staging.len() % 4 != 0 {
            staging.push(0);
        }
        let offset = staging.len();
        if array.type_ == gles11::FIXED || !aligned {
            // WebGL has no fixed-point type and needs aligned data, so convert
            // to floats (applying any normalization here).
            for v in min..=max {
                let vertex = base.add(v * stride);
                for c in 0..array.size as usize {
                    let f = read_component(vertex.add(c * tsize), array.type_, normalized);
                    staging.extend_from_slice(&f.to_ne_bytes());
                }
            }
            AttribSource::Stream {
                size: array.size,
                type_: gl3::FLOAT,
                normalized: false,
                stride: 0,
                offset,
            }
        } else {
            let count = max + 1 - min;
            staging.reserve(count * elem_size);
            for v in min..=max {
                let vertex = base.add(v * stride);
                staging.extend_from_slice(std::slice::from_raw_parts(vertex, elem_size));
            }
            AttribSource::Stream {
                size: array.size,
                type_: array.type_,
                normalized,
                stride: 0,
                offset,
            }
        }
    }

    unsafe fn draw(&mut self, mode: GLenum, first: GLint, count: GLsizei, indices: Option<(GLenum, *const GLvoid)>) {
        assert!([
            gles11::POINTS,
            gles11::LINE_STRIP,
            gles11::LINE_LOOP,
            gles11::LINES,
            gles11::TRIANGLE_STRIP,
            gles11::TRIANGLE_FAN,
            gles11::TRIANGLES
        ]
        .contains(&mode));
        if count <= 0 || !self.state.vertex_array.enabled {
            return;
        }
        let key = self.shader_key();

        with_shared(|shared| {
            // Find the range of vertices used, and copy the indices, rebased
            // to start at that range's first vertex.
            let (min, max) = if let Some((type_, ptr)) = indices {
                let index_size = match type_ {
                    gles11::UNSIGNED_BYTE => 1,
                    gles11::UNSIGNED_SHORT => 2,
                    _ => panic!("Unexpected index type {type_:#x}"),
                };
                let src: *const u8 = if self.state.element_array_buffer != 0 {
                    let data = &shared.buffer_data[&self.state.element_array_buffer];
                    assert!(ptr as usize + count as usize * index_size <= data.len());
                    data.as_ptr().add(ptr as usize)
                } else {
                    ptr.cast()
                };
                let read = |i: usize| -> usize {
                    if index_size == 1 {
                        *src.add(i) as usize
                    } else {
                        src.add(i * 2).cast::<u16>().read_unaligned() as usize
                    }
                };
                let (mut min, mut max) = (usize::MAX, 0);
                for i in 0..count as usize {
                    let index = read(i);
                    min = min.min(index);
                    max = max.max(index);
                }
                shared.index_staging.clear();
                for i in 0..count as usize {
                    let index = read(i) - min;
                    if index_size == 1 {
                        shared.index_staging.push(index as u8);
                    } else {
                        shared
                            .index_staging
                            .extend_from_slice(&(index as u16).to_ne_bytes());
                    }
                }
                (min, max)
            } else {
                (first as usize, first as usize + count as usize - 1)
            };

            let s = &*self.state;
            shared.staging.clear();
            let mut attribs: Vec<(GLuint, AttribSource)> = Vec::with_capacity(5);
            let staging = &mut shared.staging;
            let buffer_data = &shared.buffer_data;
            let mut source = |array: &ArrayState, used: bool, normalized: bool, current: [f32; 4]| {
                if used && array.enabled {
                    Self::prepare_attrib(array, normalized, min, max, staging, buffer_data)
                } else {
                    AttribSource::Constant(current)
                }
            };
            let [nx, ny, nz] = s.current_normal;
            attribs.push((
                ATTRIB_POSITION,
                source(&s.vertex_array, true, false, [0.0, 0.0, 0.0, 1.0]),
            ));
            attribs.push((ATTRIB_COLOR, source(&s.color_array, true, true, s.current_color)));
            attribs.push((
                ATTRIB_NORMAL,
                source(&s.normal_array, key.lighting, true, [nx, ny, nz, 0.0]),
            ));
            for i in 0..TEXTURE_UNITS {
                let unit = &s.units[i];
                attribs.push((
                    ATTRIB_TEXCOORD0 + i as GLuint,
                    source(&unit.array, key.units[i].is_some(), false, unit.current_texcoord),
                ));
            }

            if !shared.staging.is_empty() {
                gl3::BindBuffer(gl3::ARRAY_BUFFER, shared.stream_vertex_buffer);
                gl3::BufferData(
                    gl3::ARRAY_BUFFER,
                    shared.staging.len() as GLsizeiptr,
                    shared.staging.as_ptr().cast(),
                    gl3::STREAM_DRAW,
                );
            }
            for (loc, source) in &attribs {
                match *source {
                    AttribSource::Buffer { buffer, size, type_, normalized, stride, offset } => {
                        gl3::BindBuffer(gl3::ARRAY_BUFFER, buffer);
                        gl3::EnableVertexAttribArray(*loc);
                        gl3::VertexAttribPointer(*loc, size, type_, normalized as GLboolean, stride, offset as *const _);
                    }
                    AttribSource::Stream { size, type_, normalized, stride, offset } => {
                        gl3::BindBuffer(gl3::ARRAY_BUFFER, shared.stream_vertex_buffer);
                        gl3::EnableVertexAttribArray(*loc);
                        gl3::VertexAttribPointer(*loc, size, type_, normalized as GLboolean, stride, offset as *const _);
                    }
                    AttribSource::Constant(v) => {
                        gl3::DisableVertexAttribArray(*loc);
                        gl3::VertexAttrib4fv(*loc, v.as_ptr());
                    }
                }
            }

            if !shared.programs.contains_key(&key) {
                let program = build_program(&key);
                shared.programs.insert(key, program);
            }
            let program = &shared.programs[&key];
            gl3::UseProgram(program.program);
            self.set_uniforms(program);

            if let Some((type_, _)) = indices {
                gl3::BindBuffer(gl3::ELEMENT_ARRAY_BUFFER, shared.stream_index_buffer);
                gl3::BufferData(
                    gl3::ELEMENT_ARRAY_BUFFER,
                    shared.index_staging.len() as GLsizeiptr,
                    shared.index_staging.as_ptr().cast(),
                    gl3::STREAM_DRAW,
                );
                gl3::DrawElements(mode, count, type_, std::ptr::null());
                gl3::BindBuffer(gl3::ELEMENT_ARRAY_BUFFER, s.element_array_buffer);
            } else {
                gl3::DrawArrays(mode, 0, count);
            }
            gl3::BindBuffer(gl3::ARRAY_BUFFER, s.array_buffer);
        });
    }

    fn set_cap(&mut self, cap: GLenum, value: bool) {
        if let Some(cap_state) = self.state.native_caps.get_mut(&cap) {
            *cap_state = value;
            unsafe {
                if value {
                    gl3::Enable(cap)
                } else {
                    gl3::Disable(cap)
                }
            }
            return;
        }
        let s = &mut *self.state;
        match cap {
            gles11::ALPHA_TEST => s.alpha_test = value,
            gles11::COLOR_MATERIAL => s.color_material = value,
            gles11::FOG => s.fog = value,
            gles11::LIGHTING => s.lighting = value,
            gles11::NORMALIZE => s.normalize = value,
            gles11::RESCALE_NORMAL => s.rescale_normal = value,
            oes::POINT_SPRITE_OES => s.point_sprite = value,
            gles11::TEXTURE_2D => s.units[s.active_texture].texture_2d = value,
            _ if (gles11::LIGHT0..gles11::LIGHT0 + LIGHTS as GLenum).contains(&cap) => {
                s.light_enabled[(cap - gles11::LIGHT0) as usize] = value
            }
            _ if (gles11::CLIP_PLANE0..gles11::CLIP_PLANE0 + CLIP_PLANES as GLenum)
                .contains(&cap) =>
            {
                s.clip_plane_enabled[(cap - gles11::CLIP_PLANE0) as usize] = value
            }
            _ if EMULATED_CAPS.contains(&cap) => {
                if value && cap == gles11::COLOR_LOGIC_OP {
                    log!("Warning: GL_COLOR_LOGIC_OP is not supported, ignoring");
                }
                s.other_caps.insert(cap, value);
            }
            _ if s.array_mut(cap).is_some() => {
                log_dbg!("Tolerating glEnable/glDisable({:#x}) of client state", cap);
                s.array_mut(cap).unwrap().enabled = value;
            }
            gles11::PERSPECTIVE_CORRECTION_HINT
            | gles11::SMOOTH
            | gles11::BLEND_EQUATION_OES
            | gles11::TEXTURE => {
                log_dbg!("Tolerating glEnable/glDisable({:#x})", cap);
            }
            _ => {
                log!("Warning: unexpected glEnable/glDisable({:#x})", cap);
                s.set_error(gles11::INVALID_ENUM);
            }
        }
    }

    fn get_cap(&self, cap: GLenum) -> Option<bool> {
        let s = &*self.state;
        if let Some(&v) = s.native_caps.get(&cap) {
            return Some(v);
        }
        Some(match cap {
            gles11::ALPHA_TEST => s.alpha_test,
            gles11::COLOR_MATERIAL => s.color_material,
            gles11::FOG => s.fog,
            gles11::LIGHTING => s.lighting,
            gles11::NORMALIZE => s.normalize,
            gles11::RESCALE_NORMAL => s.rescale_normal,
            oes::POINT_SPRITE_OES => s.point_sprite,
            gles11::TEXTURE_2D => s.units[s.active_texture].texture_2d,
            gles11::VERTEX_ARRAY => s.vertex_array.enabled,
            gles11::COLOR_ARRAY => s.color_array.enabled,
            gles11::NORMAL_ARRAY => s.normal_array.enabled,
            gles11::TEXTURE_COORD_ARRAY => s.units[s.client_active_texture].array.enabled,
            oes::POINT_SIZE_ARRAY_OES => s.point_size_array.enabled,
            _ if (gles11::LIGHT0..gles11::LIGHT0 + LIGHTS as GLenum).contains(&cap) => {
                s.light_enabled[(cap - gles11::LIGHT0) as usize]
            }
            _ if (gles11::CLIP_PLANE0..gles11::CLIP_PLANE0 + CLIP_PLANES as GLenum)
                .contains(&cap) =>
            {
                s.clip_plane_enabled[(cap - gles11::CLIP_PLANE0) as usize]
            }
            _ if EMULATED_CAPS.contains(&cap) => *s.other_caps.get(&cap).unwrap_or(&false),
            _ => return None,
        })
    }

    /// Answer a `glGet*` query. Returns the values and whether they are
    /// integers (as opposed to floats).
    fn query(&self, pname: GLenum) -> Option<(Vec<f32>, bool)> {
        if let Some(v) = self.get_cap(pname) {
            return Some((vec![v as u8 as f32], true));
        }
        let s = &*self.state;
        let i = |v: &[i64]| Some((v.iter().map(|&x| x as f32).collect(), true));
        let f = |v: &[f32]| Some((v.to_vec(), false));
        let array_query = |array: &ArrayState, what: u8| -> Option<(Vec<f32>, bool)> {
            match what {
                0 => i(&[array.size as i64]),
                1 => i(&[array.type_ as i64]),
                2 => i(&[array.stride as i64]),
                _ => i(&[array.buffer as i64]),
            }
        };
        let tex = &s.units[s.client_active_texture].array;
        match pname {
            gles11::ACTIVE_TEXTURE => i(&[(gles11::TEXTURE0 as usize + s.active_texture) as i64]),
            gles11::CLIENT_ACTIVE_TEXTURE => {
                i(&[(gles11::TEXTURE0 as usize + s.client_active_texture) as i64])
            }
            gles11::ALIASED_POINT_SIZE_RANGE | gles11::SMOOTH_POINT_SIZE_RANGE => f(&[1.0, 64.0]),
            gles11::ALIASED_LINE_WIDTH_RANGE | gles11::SMOOTH_LINE_WIDTH_RANGE => f(&[1.0, 1.0]),
            gles11::RED_BITS | gles11::GREEN_BITS | gles11::BLUE_BITS | gles11::ALPHA_BITS => {
                i(&[8])
            }
            gles11::DEPTH_BITS => i(&[24]),
            gles11::STENCIL_BITS => i(&[8]),
            gles11::SUBPIXEL_BITS => i(&[4]),
            gles11::ALPHA_TEST_FUNC => i(&[s.alpha_func as i64]),
            gles11::ALPHA_TEST_REF => f(&[s.alpha_ref]),
            gles11::ARRAY_BUFFER_BINDING => i(&[s.array_buffer as i64]),
            gles11::ELEMENT_ARRAY_BUFFER_BINDING => i(&[s.element_array_buffer as i64]),
            gles11::BLEND_SRC => i(&[s.blend_func[0] as i64]),
            gles11::BLEND_DST => i(&[s.blend_func[1] as i64]),
            gles11::COLOR_CLEAR_VALUE => f(&s.clear_color),
            gles11::COLOR_WRITEMASK => i(&s.color_mask.map(|x| x as i64)),
            gles11::CULL_FACE_MODE => i(&[s.cull_face_mode as i64]),
            gles11::CURRENT_COLOR => f(&s.current_color),
            gles11::CURRENT_NORMAL => f(&s.current_normal),
            gles11::CURRENT_TEXTURE_COORDS => f(&s.units[s.active_texture].current_texcoord),
            gles11::DEPTH_CLEAR_VALUE => f(&[s.clear_depth]),
            gles11::DEPTH_FUNC => i(&[s.depth_func as i64]),
            gles11::DEPTH_RANGE => f(&s.depth_range),
            gles11::DEPTH_WRITEMASK => i(&[s.depth_mask as i64]),
            gles11::FOG_COLOR => f(&s.fog_color),
            gles11::FOG_DENSITY => f(&[s.fog_density]),
            gles11::FOG_START => f(&[s.fog_start]),
            gles11::FOG_END => f(&[s.fog_end]),
            gles11::FOG_MODE => i(&[s.fog_mode as i64]),
            gles11::FOG_HINT
            | gles11::GENERATE_MIPMAP_HINT
            | gles11::LINE_SMOOTH_HINT
            | gles11::PERSPECTIVE_CORRECTION_HINT
            | gles11::POINT_SMOOTH_HINT => {
                i(&[*s.hints.get(&pname).unwrap_or(&gles11::DONT_CARE) as i64])
            }
            gles11::FRONT_FACE => i(&[s.front_face as i64]),
            gles11::LIGHT_MODEL_AMBIENT => f(&s.light_model_ambient),
            gles11::LIGHT_MODEL_TWO_SIDE => i(&[s.light_model_two_side as i64]),
            gles11::LINE_WIDTH => f(&[s.line_width]),
            gles11::LOGIC_OP_MODE => i(&[s.logic_op as i64]),
            gles11::MATRIX_MODE => i(&[s.matrix_mode as i64]),
            gles11::MAX_CLIP_PLANES => i(&[CLIP_PLANES as i64]),
            gles11::MAX_LIGHTS => i(&[LIGHTS as i64]),
            gles11::MAX_MODELVIEW_STACK_DEPTH => i(&[MODELVIEW_STACK_DEPTH as i64]),
            gles11::MAX_PROJECTION_STACK_DEPTH => i(&[PROJECTION_STACK_DEPTH as i64]),
            gles11::MAX_TEXTURE_STACK_DEPTH => i(&[TEXTURE_STACK_DEPTH as i64]),
            gles11::MAX_TEXTURE_SIZE | gles11::MAX_RENDERBUFFER_SIZE_OES => i(&[2048]),
            gles11::MAX_TEXTURE_UNITS => i(&[TEXTURE_UNITS as i64]),
            gles11::MAX_VIEWPORT_DIMS => i(&[2048, 2048]),
            gles11::MAX_TEXTURE_MAX_ANISOTROPY_EXT => f(&[2.0]),
            gles11::MAX_TEXTURE_LOD_BIAS_EXT => f(&[2.0]),
            gles11::MAX_PALETTE_MATRICES_OES => i(&[9]),
            gles11::MAX_VERTEX_UNITS_OES => i(&[3]),
            gles11::MODELVIEW_MATRIX => f(s.modelview()),
            gles11::PROJECTION_MATRIX => f(s.projection_stack.last().unwrap()),
            gles11::TEXTURE_MATRIX => f(s.units[s.active_texture].matrix_stack.last().unwrap()),
            gles11::MODELVIEW_STACK_DEPTH => i(&[s.modelview_stack.len() as i64]),
            gles11::PROJECTION_STACK_DEPTH => i(&[s.projection_stack.len() as i64]),
            gles11::TEXTURE_STACK_DEPTH => {
                i(&[s.units[s.active_texture].matrix_stack.len() as i64])
            }
            gles11::PACK_ALIGNMENT => i(&[s.pack_alignment as i64]),
            gles11::UNPACK_ALIGNMENT => i(&[s.unpack_alignment as i64]),
            gles11::POINT_SIZE => f(&[s.point_size]),
            gles11::POINT_SIZE_MIN => f(&[s.point_size_min]),
            gles11::POINT_SIZE_MAX => f(&[s.point_size_max]),
            gles11::POINT_FADE_THRESHOLD_SIZE => f(&[s.point_fade_threshold]),
            gles11::POINT_DISTANCE_ATTENUATION => f(&s.point_attenuation),
            gles11::POLYGON_OFFSET_FACTOR => f(&[s.polygon_offset[0]]),
            gles11::POLYGON_OFFSET_UNITS => f(&[s.polygon_offset[1]]),
            gles11::SAMPLE_BUFFERS | gles11::SAMPLES => i(&[0]),
            gles11::SAMPLE_COVERAGE_VALUE => f(&[s.sample_coverage.0]),
            gles11::SAMPLE_COVERAGE_INVERT => i(&[s.sample_coverage.1 as i64]),
            gles11::SCISSOR_BOX => i(&s.scissor.map(|x| x as i64)),
            gles11::VIEWPORT => i(&s.viewport.map(|x| x as i64)),
            gles11::SHADE_MODEL => i(&[s.shade_model as i64]),
            gles11::STENCIL_CLEAR_VALUE => i(&[s.clear_stencil as i64]),
            gles11::STENCIL_FUNC => i(&[s.stencil_func.0 as i64]),
            gles11::STENCIL_REF => i(&[s.stencil_func.1 as i64]),
            gles11::STENCIL_VALUE_MASK => i(&[s.stencil_func.2 as i64]),
            gles11::STENCIL_FAIL => i(&[s.stencil_op[0] as i64]),
            gles11::STENCIL_PASS_DEPTH_FAIL => i(&[s.stencil_op[1] as i64]),
            gles11::STENCIL_PASS_DEPTH_PASS => i(&[s.stencil_op[2] as i64]),
            gles11::STENCIL_WRITEMASK => i(&[s.stencil_writemask as i64]),
            gles11::TEXTURE_BINDING_2D => i(&[s.units[s.active_texture].binding as i64]),
            gles11::FRAMEBUFFER_BINDING_OES => i(&[s.framebuffer as i64]),
            gles11::RENDERBUFFER_BINDING_OES => i(&[s.renderbuffer as i64]),
            oes::IMPLEMENTATION_COLOR_READ_FORMAT_OES => i(&[gles11::RGBA as i64]),
            oes::IMPLEMENTATION_COLOR_READ_TYPE_OES => i(&[gles11::UNSIGNED_BYTE as i64]),
            gles11::NUM_COMPRESSED_TEXTURE_FORMATS => i(&[4]),
            gles11::COMPRESSED_TEXTURE_FORMATS => i(&[
                gles11::COMPRESSED_RGB_PVRTC_4BPPV1_IMG as i64,
                gles11::COMPRESSED_RGB_PVRTC_2BPPV1_IMG as i64,
                gles11::COMPRESSED_RGBA_PVRTC_4BPPV1_IMG as i64,
                gles11::COMPRESSED_RGBA_PVRTC_2BPPV1_IMG as i64,
            ]),
            gles11::VERTEX_ARRAY_SIZE => array_query(&s.vertex_array, 0),
            gles11::VERTEX_ARRAY_TYPE => array_query(&s.vertex_array, 1),
            gles11::VERTEX_ARRAY_STRIDE => array_query(&s.vertex_array, 2),
            gles11::VERTEX_ARRAY_BUFFER_BINDING => array_query(&s.vertex_array, 3),
            gles11::COLOR_ARRAY_SIZE => array_query(&s.color_array, 0),
            gles11::COLOR_ARRAY_TYPE => array_query(&s.color_array, 1),
            gles11::COLOR_ARRAY_STRIDE => array_query(&s.color_array, 2),
            gles11::COLOR_ARRAY_BUFFER_BINDING => array_query(&s.color_array, 3),
            gles11::NORMAL_ARRAY_TYPE => array_query(&s.normal_array, 1),
            gles11::NORMAL_ARRAY_STRIDE => array_query(&s.normal_array, 2),
            gles11::NORMAL_ARRAY_BUFFER_BINDING => array_query(&s.normal_array, 3),
            gles11::TEXTURE_COORD_ARRAY_SIZE => array_query(tex, 0),
            gles11::TEXTURE_COORD_ARRAY_TYPE => array_query(tex, 1),
            gles11::TEXTURE_COORD_ARRAY_STRIDE => array_query(tex, 2),
            gles11::TEXTURE_COORD_ARRAY_BUFFER_BINDING => array_query(tex, 3),
            _ => None,
        }
    }

    /// Convert pixels from `format` to something WebGL accepts. Returns the
    /// replacement format and data if conversion was needed.
    unsafe fn convert_pixels(
        &self,
        width: GLsizei,
        height: GLsizei,
        format: GLenum,
        type_: GLenum,
        pixels: *const GLvoid,
    ) -> Option<(GLenum, Vec<u8>)> {
        if format != gles11::BGRA_EXT || pixels.is_null() {
            return None;
        }
        assert_eq!(type_, gles11::UNSIGNED_BYTE);
        let align = self.state.unpack_alignment as usize;
        let row_bytes = width as usize * 4;
        let row_stride = row_bytes.div_ceil(align) * align;
        let mut out = Vec::with_capacity(row_bytes * height as usize);
        for y in 0..height as usize {
            let row = std::slice::from_raw_parts(pixels.cast::<u8>().add(y * row_stride), row_bytes);
            for px in row.chunks_exact(4) {
                out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        Some((gles11::RGBA, out))
    }

    fn maybe_generate_mipmap(&self, level: GLint) {
        let binding = self.state.units[self.state.active_texture].binding;
        if level == 0 && with_shared(|s| s.generate_mipmap.get(&binding).copied().unwrap_or(false)) {
            unsafe { gl3::GenerateMipmap(gl3::TEXTURE_2D) };
        }
    }

    fn buffer_for_target(&self, target: GLenum) -> GLuint {
        match target {
            gles11::ARRAY_BUFFER => self.state.array_buffer,
            gles11::ELEMENT_ARRAY_BUFFER => self.state.element_array_buffer,
            _ => panic!("Unexpected buffer target {target:#x}"),
        }
    }

    fn light_mut(&mut self, light: GLenum) -> &mut Light {
        let index = light.checked_sub(gles11::LIGHT0).expect("Bad light") as usize;
        &mut self.state.lights[index]
    }
}

impl GLES for GLES1OnGLES3<'_> {
    unsafe fn driver_description(&self) -> String {
        let get = |name| {
            let ptr = gl3::GetString(name);
            if ptr.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(ptr.cast()).to_string_lossy().into_owned()
            }
        };
        format!(
            "{} / {} / {}",
            get(gl3::VERSION),
            get(gl3::VENDOR),
            get(gl3::RENDERER)
        )
    }

    // Generic state manipulation
    unsafe fn GetError(&mut self) -> GLenum {
        let error = std::mem::replace(&mut self.state.error, gles11::NO_ERROR);
        if error != gles11::NO_ERROR {
            error
        } else {
            gl3::GetError()
        }
    }
    unsafe fn Enable(&mut self, cap: GLenum) {
        self.set_cap(cap, true)
    }
    unsafe fn IsEnabled(&mut self, cap: GLenum) -> GLboolean {
        match self.get_cap(cap) {
            Some(v) => v as GLboolean,
            None => {
                self.state.set_error(gles11::INVALID_ENUM);
                gles11::FALSE
            }
        }
    }
    unsafe fn Disable(&mut self, cap: GLenum) {
        self.set_cap(cap, false)
    }
    unsafe fn ClientActiveTexture(&mut self, texture: GLenum) {
        let unit = texture.wrapping_sub(gles11::TEXTURE0) as usize;
        assert!(unit < TEXTURE_UNITS, "Bad glClientActiveTexture({texture:#x})");
        self.state.client_active_texture = unit;
    }
    unsafe fn EnableClientState(&mut self, array: GLenum) {
        match self.state.array_mut(array) {
            Some(a) => a.enabled = true,
            None => self.set_cap(array, true),
        }
    }
    unsafe fn DisableClientState(&mut self, array: GLenum) {
        match self.state.array_mut(array) {
            Some(a) => a.enabled = false,
            None => self.set_cap(array, false),
        }
    }
    unsafe fn GetBooleanv(&mut self, pname: GLenum, params: *mut GLboolean) {
        let (values, _) = self
            .query(pname)
            .unwrap_or_else(|| panic!("Unhandled glGetBooleanv({pname:#x})"));
        for (i, v) in values.into_iter().enumerate() {
            *params.add(i) = (v != 0.0) as GLboolean;
        }
    }
    unsafe fn GetFloatv(&mut self, pname: GLenum, params: *mut GLfloat) {
        let (values, _) = self
            .query(pname)
            .unwrap_or_else(|| panic!("Unhandled glGetFloatv({pname:#x})"));
        for (i, v) in values.into_iter().enumerate() {
            *params.add(i) = v;
        }
    }
    unsafe fn GetIntegerv(&mut self, pname: GLenum, params: *mut GLint) {
        let (values, is_int) = self
            .query(pname)
            .unwrap_or_else(|| panic!("Unhandled glGetIntegerv({pname:#x})"));
        let is_color = matches!(
            pname,
            gles11::COLOR_CLEAR_VALUE
                | gles11::CURRENT_COLOR
                | gles11::FOG_COLOR
                | gles11::LIGHT_MODEL_AMBIENT
                | gles11::DEPTH_RANGE
                | gles11::DEPTH_CLEAR_VALUE
                | gles11::ALPHA_TEST_REF
        );
        for (i, v) in values.into_iter().enumerate() {
            *params.add(i) = if is_int {
                v as GLint
            } else if is_color {
                // Colors map [-1, 1] to the full integer range.
                (v as f64 * GLint::MAX as f64) as GLint
            } else {
                v.round() as GLint
            };
        }
    }
    unsafe fn GetTexEnviv(&mut self, target: GLenum, pname: GLenum, params: *mut GLint) {
        assert_eq!(target, gles11::TEXTURE_ENV);
        let unit = &self.state.units[self.state.active_texture];
        let env = &unit.env;
        *params = match pname {
            gles11::TEXTURE_ENV_MODE => env.mode,
            gles11::COMBINE_RGB => env.combine_rgb,
            gles11::COMBINE_ALPHA => env.combine_alpha,
            gles11::SRC0_RGB => env.src_rgb[0],
            gles11::SRC1_RGB => env.src_rgb[1],
            gles11::SRC2_RGB => env.src_rgb[2],
            gles11::SRC0_ALPHA => env.src_alpha[0],
            gles11::SRC1_ALPHA => env.src_alpha[1],
            gles11::SRC2_ALPHA => env.src_alpha[2],
            gles11::OPERAND0_RGB => env.operand_rgb[0],
            gles11::OPERAND1_RGB => env.operand_rgb[1],
            gles11::OPERAND2_RGB => env.operand_rgb[2],
            gles11::OPERAND0_ALPHA => env.operand_alpha[0],
            gles11::OPERAND1_ALPHA => env.operand_alpha[1],
            gles11::OPERAND2_ALPHA => env.operand_alpha[2],
            oes::COORD_REPLACE_OES => unit.coord_replace as GLenum,
            gles11::RGB_SCALE => unit.rgb_scale as GLenum,
            gles11::ALPHA_SCALE => unit.alpha_scale as GLenum,
            _ => panic!("Unhandled glGetTexEnviv({pname:#x})"),
        } as GLint;
    }
    unsafe fn GetTexEnvfv(&mut self, target: GLenum, pname: GLenum, params: *mut GLfloat) {
        assert_eq!(target, gles11::TEXTURE_ENV);
        let unit = &self.state.units[self.state.active_texture];
        match pname {
            gles11::TEXTURE_ENV_COLOR => {
                std::ptr::copy_nonoverlapping(unit.env_color.as_ptr(), params, 4)
            }
            gles11::RGB_SCALE => *params = unit.rgb_scale,
            gles11::ALPHA_SCALE => *params = unit.alpha_scale,
            _ => {
                let mut i = 0;
                self.GetTexEnviv(target, pname, &mut i);
                *params = i as GLfloat;
            }
        }
    }
    unsafe fn GetTexParameteriv(&mut self, target: GLenum, pname: GLenum, params: *mut GLint) {
        assert_eq!(target, gles11::TEXTURE_2D);
        if pname == gles11::GENERATE_MIPMAP {
            let binding = self.state.units[self.state.active_texture].binding;
            *params = with_shared(|s| s.generate_mipmap.get(&binding).copied().unwrap_or(false))
                as GLint;
        } else {
            gl3::GetTexParameteriv(target, pname, params);
        }
    }
    unsafe fn GetPointerv(&mut self, pname: GLenum, params: *mut *const GLvoid) {
        let s = &*self.state;
        *params = match pname {
            gles11::VERTEX_ARRAY_POINTER => s.vertex_array.pointer,
            gles11::COLOR_ARRAY_POINTER => s.color_array.pointer,
            gles11::NORMAL_ARRAY_POINTER => s.normal_array.pointer,
            gles11::TEXTURE_COORD_ARRAY_POINTER => s.units[s.client_active_texture].array.pointer,
            oes::POINT_SIZE_ARRAY_POINTER_OES => s.point_size_array.pointer,
            _ => panic!("Unhandled glGetPointerv({pname:#x})"),
        };
    }
    unsafe fn Hint(&mut self, target: GLenum, mode: GLenum) {
        self.state.hints.insert(target, mode);
        if target == gles11::GENERATE_MIPMAP_HINT
            && [gles11::FASTEST, gles11::NICEST, gles11::DONT_CARE].contains(&mode)
        {
            gl3::Hint(target, mode);
        }
    }
    unsafe fn Finish(&mut self) {
        gl3::Finish();
    }
    unsafe fn Flush(&mut self) {
        gl3::Flush();
    }
    unsafe fn GetString(&mut self, name: GLenum) -> *const GLubyte {
        gl3::GetString(name)
    }

    // Other state manipulation
    unsafe fn AlphaFunc(&mut self, func: GLenum, ref_: GLclampf) {
        self.state.alpha_func = func;
        self.state.alpha_ref = ref_.clamp(0.0, 1.0);
    }
    unsafe fn AlphaFuncx(&mut self, func: GLenum, ref_: GLclampx) {
        self.AlphaFunc(func, fixed_to_float(ref_))
    }
    unsafe fn BlendFunc(&mut self, sfactor: GLenum, dfactor: GLenum) {
        self.state.blend_func = [sfactor, dfactor];
        gl3::BlendFunc(sfactor, dfactor);
    }
    unsafe fn BlendEquationOES(&mut self, mode: GLenum) {
        self.state.blend_equation = mode;
        gl3::BlendEquation(mode);
    }
    unsafe fn ColorMask(
        &mut self,
        red: GLboolean,
        green: GLboolean,
        blue: GLboolean,
        alpha: GLboolean,
    ) {
        self.state.color_mask = [red, green, blue, alpha];
        gl3::ColorMask(red, green, blue, alpha);
    }
    unsafe fn ClipPlanef(&mut self, plane: GLenum, equation: *const GLfloat) {
        let index = plane.wrapping_sub(gles11::CLIP_PLANE0) as usize;
        assert!(index < CLIP_PLANES, "Bad clip plane {plane:#x}");
        let eq: [f32; 4] = std::array::from_fn(|i| *equation.add(i));
        // Planes are specified in object space and stored in eye space:
        // p_eye = p * inverse(modelview).
        let inv = mat_inverse(self.state.modelview());
        let mut eye = [0.0; 4];
        for (col, out) in eye.iter_mut().enumerate() {
            *out = (0..4).map(|k| eq[k] * inv[col * 4 + k]).sum();
        }
        self.state.clip_planes[index] = eye;
    }
    unsafe fn ClipPlanex(&mut self, plane: GLenum, equation: *const GLfixed) {
        let eq: [f32; 4] = std::array::from_fn(|i| fixed_to_float(*equation.add(i)));
        self.ClipPlanef(plane, eq.as_ptr())
    }
    unsafe fn CullFace(&mut self, mode: GLenum) {
        self.state.cull_face_mode = mode;
        gl3::CullFace(mode);
    }
    unsafe fn DepthFunc(&mut self, func: GLenum) {
        self.state.depth_func = func;
        gl3::DepthFunc(func)
    }
    unsafe fn DepthMask(&mut self, flag: GLboolean) {
        self.state.depth_mask = flag;
        gl3::DepthMask(flag)
    }
    unsafe fn FrontFace(&mut self, mode: GLenum) {
        self.state.front_face = mode;
        gl3::FrontFace(mode);
    }
    unsafe fn DepthRangef(&mut self, near: GLclampf, far: GLclampf) {
        self.state.depth_range = [near, far];
        gl3::DepthRangef(near, far)
    }
    unsafe fn DepthRangex(&mut self, near: GLclampx, far: GLclampx) {
        self.DepthRangef(fixed_to_float(near), fixed_to_float(far))
    }
    unsafe fn PolygonOffset(&mut self, factor: GLfloat, units: GLfloat) {
        self.state.polygon_offset = [factor, units];
        gl3::PolygonOffset(factor, units)
    }
    unsafe fn PolygonOffsetx(&mut self, factor: GLfixed, units: GLfixed) {
        self.PolygonOffset(fixed_to_float(factor), fixed_to_float(units))
    }
    unsafe fn SampleCoverage(&mut self, value: GLclampf, invert: GLboolean) {
        self.state.sample_coverage = (value, invert);
        gl3::SampleCoverage(value, invert)
    }
    unsafe fn SampleCoveragex(&mut self, value: GLclampx, invert: GLboolean) {
        self.SampleCoverage(fixed_to_float(value), invert)
    }
    unsafe fn ShadeModel(&mut self, mode: GLenum) {
        assert!(mode == gles11::FLAT || mode == gles11::SMOOTH);
        self.state.shade_model = mode;
    }
    unsafe fn Scissor(&mut self, x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
        self.state.scissor = [x, y, width, height];
        gl3::Scissor(x, y, width, height)
    }
    unsafe fn Viewport(&mut self, x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
        self.state.viewport = [x, y, width, height];
        gl3::Viewport(x, y, width, height)
    }
    unsafe fn LineWidth(&mut self, val: GLfloat) {
        // WebGL only guarantees a width of 1.
        self.state.line_width = val;
    }
    unsafe fn LineWidthx(&mut self, val: GLfixed) {
        self.LineWidth(fixed_to_float(val))
    }
    unsafe fn StencilFunc(&mut self, func: GLenum, ref_: GLint, mask: GLuint) {
        self.state.stencil_func = (func, ref_, mask);
        gl3::StencilFunc(func, ref_, mask)
    }
    unsafe fn StencilOp(&mut self, sfail: GLenum, dpfail: GLenum, dppass: GLenum) {
        self.state.stencil_op = [sfail, dpfail, dppass];
        gl3::StencilOp(sfail, dpfail, dppass)
    }
    unsafe fn StencilMask(&mut self, mask: GLuint) {
        self.state.stencil_writemask = mask;
        gl3::StencilMask(mask)
    }
    unsafe fn LogicOp(&mut self, opcode: GLenum) {
        self.state.logic_op = opcode;
    }

    // Points
    unsafe fn PointSize(&mut self, size: GLfloat) {
        self.state.point_size = size;
    }
    unsafe fn PointSizex(&mut self, size: GLfixed) {
        self.PointSize(fixed_to_float(size))
    }
    unsafe fn PointParameterf(&mut self, pname: GLenum, param: GLfloat) {
        match pname {
            gles11::POINT_SIZE_MIN => self.state.point_size_min = param,
            gles11::POINT_SIZE_MAX => self.state.point_size_max = param,
            gles11::POINT_FADE_THRESHOLD_SIZE => self.state.point_fade_threshold = param,
            _ => panic!("Unhandled glPointParameterf({pname:#x})"),
        }
    }
    unsafe fn PointParameterx(&mut self, pname: GLenum, param: GLfixed) {
        self.PointParameterf(pname, fixed_to_float(param))
    }
    unsafe fn PointParameterfv(&mut self, pname: GLenum, params: *const GLfloat) {
        if pname == gles11::POINT_DISTANCE_ATTENUATION {
            self.state.point_attenuation = std::array::from_fn(|i| *params.add(i));
        } else {
            self.PointParameterf(pname, *params)
        }
    }
    unsafe fn PointParameterxv(&mut self, pname: GLenum, params: *const GLfixed) {
        let count = if pname == gles11::POINT_DISTANCE_ATTENUATION { 3 } else { 1 };
        let floats: Vec<f32> = (0..count).map(|i| fixed_to_float(*params.add(i))).collect();
        self.PointParameterfv(pname, floats.as_ptr())
    }

    // Lighting and materials
    unsafe fn Fogf(&mut self, pname: GLenum, param: GLfloat) {
        let s = &mut *self.state;
        match pname {
            gles11::FOG_MODE => s.fog_mode = param as GLenum,
            gles11::FOG_DENSITY => s.fog_density = param,
            gles11::FOG_START => s.fog_start = param,
            gles11::FOG_END => s.fog_end = param,
            _ => panic!("Unhandled glFogf({pname:#x})"),
        }
    }
    unsafe fn Fogx(&mut self, pname: GLenum, param: GLfixed) {
        if pname == gles11::FOG_MODE {
            // Enum values are passed as-is, not as fixed-point.
            self.state.fog_mode = param as GLenum;
        } else {
            self.Fogf(pname, fixed_to_float(param))
        }
    }
    unsafe fn Fogfv(&mut self, pname: GLenum, params: *const GLfloat) {
        if pname == gles11::FOG_COLOR {
            self.state.fog_color = std::array::from_fn(|i| *params.add(i));
        } else {
            self.Fogf(pname, *params)
        }
    }
    unsafe fn Fogxv(&mut self, pname: GLenum, params: *const GLfixed) {
        if pname == gles11::FOG_COLOR {
            self.state.fog_color = std::array::from_fn(|i| fixed_to_float(*params.add(i)));
        } else {
            self.Fogx(pname, *params)
        }
    }
    unsafe fn Lightf(&mut self, light: GLenum, pname: GLenum, param: GLfloat) {
        let l = self.light_mut(light);
        match pname {
            gles11::SPOT_EXPONENT => l.spot_exponent = param,
            gles11::SPOT_CUTOFF => l.spot_cutoff = param,
            gles11::CONSTANT_ATTENUATION => l.attenuation[0] = param,
            gles11::LINEAR_ATTENUATION => l.attenuation[1] = param,
            gles11::QUADRATIC_ATTENUATION => l.attenuation[2] = param,
            _ => panic!("Unhandled glLightf({pname:#x})"),
        }
    }
    unsafe fn Lightx(&mut self, light: GLenum, pname: GLenum, param: GLfixed) {
        self.Lightf(light, pname, fixed_to_float(param))
    }
    unsafe fn Lightfv(&mut self, light: GLenum, pname: GLenum, params: *const GLfloat) {
        let v4: [f32; 4] = match pname {
            gles11::AMBIENT | gles11::DIFFUSE | gles11::SPECULAR | gles11::POSITION => {
                std::array::from_fn(|i| *params.add(i))
            }
            gles11::SPOT_DIRECTION => [*params, *params.add(1), *params.add(2), 0.0],
            _ => return self.Lightf(light, pname, *params),
        };
        let mv = *self.state.modelview();
        let l = self.light_mut(light);
        match pname {
            gles11::AMBIENT => l.ambient = v4,
            gles11::DIFFUSE => l.diffuse = v4,
            gles11::SPECULAR => l.specular = v4,
            // Positions and directions are transformed into eye space when
            // they are specified.
            gles11::POSITION => l.position = mat_transform(&mv, v4),
            gles11::SPOT_DIRECTION => {
                let [x, y, z, _] = mat_transform(&mv, v4);
                l.spot_direction = [x, y, z];
            }
            _ => unreachable!(),
        }
    }
    unsafe fn Lightxv(&mut self, light: GLenum, pname: GLenum, params: *const GLfixed) {
        let count = match pname {
            gles11::AMBIENT | gles11::DIFFUSE | gles11::SPECULAR | gles11::POSITION => 4,
            gles11::SPOT_DIRECTION => 3,
            _ => 1,
        };
        let floats: Vec<f32> = (0..count).map(|i| fixed_to_float(*params.add(i))).collect();
        self.Lightfv(light, pname, floats.as_ptr())
    }
    unsafe fn LightModelf(&mut self, pname: GLenum, param: GLfloat) {
        assert_eq!(pname, gles11::LIGHT_MODEL_TWO_SIDE);
        self.state.light_model_two_side = param != 0.0;
    }
    unsafe fn LightModelx(&mut self, pname: GLenum, param: GLfixed) {
        self.LightModelf(pname, param as GLfloat)
    }
    unsafe fn LightModelfv(&mut self, pname: GLenum, params: *const GLfloat) {
        if pname == gles11::LIGHT_MODEL_AMBIENT {
            self.state.light_model_ambient = std::array::from_fn(|i| *params.add(i));
        } else {
            self.LightModelf(pname, *params)
        }
    }
    unsafe fn LightModelxv(&mut self, pname: GLenum, params: *const GLfixed) {
        if pname == gles11::LIGHT_MODEL_AMBIENT {
            self.state.light_model_ambient = std::array::from_fn(|i| fixed_to_float(*params.add(i)));
        } else {
            self.LightModelx(pname, *params)
        }
    }
    unsafe fn Materialf(&mut self, face: GLenum, pname: GLenum, param: GLfloat) {
        assert_eq!(face, gles11::FRONT_AND_BACK);
        assert_eq!(pname, gles11::SHININESS);
        self.state.material.shininess = param;
    }
    unsafe fn Materialx(&mut self, face: GLenum, pname: GLenum, param: GLfixed) {
        self.Materialf(face, pname, fixed_to_float(param))
    }
    unsafe fn Materialfv(&mut self, face: GLenum, pname: GLenum, params: *const GLfloat) {
        assert_eq!(face, gles11::FRONT_AND_BACK);
        if pname == gles11::SHININESS {
            return self.Materialf(face, pname, *params);
        }
        let v: [f32; 4] = std::array::from_fn(|i| *params.add(i));
        let m = &mut self.state.material;
        match pname {
            gles11::AMBIENT => m.ambient = v,
            gles11::DIFFUSE => m.diffuse = v,
            gles11::SPECULAR => m.specular = v,
            gles11::EMISSION => m.emission = v,
            gles11::AMBIENT_AND_DIFFUSE => {
                m.ambient = v;
                m.diffuse = v;
            }
            _ => panic!("Unhandled glMaterialfv({pname:#x})"),
        }
    }
    unsafe fn Materialxv(&mut self, face: GLenum, pname: GLenum, params: *const GLfixed) {
        let count = if pname == gles11::SHININESS { 1 } else { 4 };
        let floats: Vec<f32> = (0..count).map(|i| fixed_to_float(*params.add(i))).collect();
        self.Materialfv(face, pname, floats.as_ptr())
    }

    // Buffers
    unsafe fn IsBuffer(&mut self, buffer: GLuint) -> GLboolean {
        gl3::IsBuffer(buffer)
    }
    unsafe fn GenBuffers(&mut self, n: GLsizei, buffers: *mut GLuint) {
        gl3::GenBuffers(n, buffers)
    }
    unsafe fn DeleteBuffers(&mut self, n: GLsizei, buffers: *const GLuint) {
        for i in 0..n as usize {
            let buffer = *buffers.add(i);
            with_shared(|s| s.buffer_data.remove(&buffer));
            if self.state.array_buffer == buffer {
                self.state.array_buffer = 0;
            }
            if self.state.element_array_buffer == buffer {
                self.state.element_array_buffer = 0;
            }
        }
        gl3::DeleteBuffers(n, buffers)
    }
    unsafe fn BindBuffer(&mut self, target: GLenum, buffer: GLuint) {
        match target {
            gles11::ARRAY_BUFFER => self.state.array_buffer = buffer,
            gles11::ELEMENT_ARRAY_BUFFER => self.state.element_array_buffer = buffer,
            _ => panic!("Unexpected buffer target {target:#x}"),
        }
        gl3::BindBuffer(target, buffer)
    }
    unsafe fn BufferData(
        &mut self,
        target: GLenum,
        size: GLsizeiptr,
        data: *const GLvoid,
        usage: GLenum,
    ) {
        let buffer = self.buffer_for_target(target);
        let bytes = if data.is_null() {
            vec![0u8; size as usize]
        } else {
            std::slice::from_raw_parts(data.cast::<u8>(), size as usize).to_vec()
        };
        gl3::BufferData(target, size, bytes.as_ptr().cast(), usage);
        with_shared(|s| s.buffer_data.insert(buffer, bytes));
    }
    unsafe fn BufferSubData(
        &mut self,
        target: GLenum,
        offset: GLintptr,
        size: GLsizeiptr,
        data: *const GLvoid,
    ) {
        let buffer = self.buffer_for_target(target);
        let src = std::slice::from_raw_parts(data.cast::<u8>(), size as usize);
        with_shared(|s| {
            let shadow = s.buffer_data.get_mut(&buffer).expect("BufferSubData on empty buffer");
            shadow[offset as usize..][..size as usize].copy_from_slice(src);
        });
        gl3::BufferSubData(target, offset, size, data)
    }

    // Non-pointers
    unsafe fn Color4f(&mut self, red: GLfloat, green: GLfloat, blue: GLfloat, alpha: GLfloat) {
        self.state.current_color = [red, green, blue, alpha];
    }
    unsafe fn Color4x(&mut self, red: GLfixed, green: GLfixed, blue: GLfixed, alpha: GLfixed) {
        self.Color4f(
            fixed_to_float(red),
            fixed_to_float(green),
            fixed_to_float(blue),
            fixed_to_float(alpha),
        )
    }
    unsafe fn Color4ub(&mut self, red: GLubyte, green: GLubyte, blue: GLubyte, alpha: GLubyte) {
        self.Color4f(
            red as f32 / 255.0,
            green as f32 / 255.0,
            blue as f32 / 255.0,
            alpha as f32 / 255.0,
        )
    }
    unsafe fn Normal3f(&mut self, nx: GLfloat, ny: GLfloat, nz: GLfloat) {
        self.state.current_normal = [nx, ny, nz];
    }
    unsafe fn Normal3x(&mut self, nx: GLfixed, ny: GLfixed, nz: GLfixed) {
        self.Normal3f(fixed_to_float(nx), fixed_to_float(ny), fixed_to_float(nz))
    }

    // Pointers
    unsafe fn ColorPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        assert!(size == 4);
        self.set_pointer(gles11::COLOR_ARRAY, size, type_, stride, pointer)
    }
    unsafe fn NormalPointer(&mut self, type_: GLenum, stride: GLsizei, pointer: *const GLvoid) {
        self.set_pointer(gles11::NORMAL_ARRAY, 3, type_, stride, pointer)
    }
    unsafe fn TexCoordPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        assert!((2..=4).contains(&size));
        self.set_pointer(gles11::TEXTURE_COORD_ARRAY, size, type_, stride, pointer)
    }
    unsafe fn VertexPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        assert!((2..=4).contains(&size));
        self.set_pointer(gles11::VERTEX_ARRAY, size, type_, stride, pointer)
    }

    // Drawing
    unsafe fn DrawArrays(&mut self, mode: GLenum, first: GLint, count: GLsizei) {
        self.draw(mode, first, count, None)
    }
    unsafe fn DrawElements(
        &mut self,
        mode: GLenum,
        count: GLsizei,
        type_: GLenum,
        indices: *const GLvoid,
    ) {
        self.draw(mode, 0, count, Some((type_, indices)))
    }

    // Clearing
    unsafe fn Clear(&mut self, mask: GLbitfield) {
        gl3::Clear(mask)
    }
    unsafe fn ClearColor(
        &mut self,
        red: GLclampf,
        green: GLclampf,
        blue: GLclampf,
        alpha: GLclampf,
    ) {
        self.state.clear_color = [red, green, blue, alpha];
        gl3::ClearColor(red, green, blue, alpha)
    }
    unsafe fn ClearColorx(
        &mut self,
        red: GLclampx,
        green: GLclampx,
        blue: GLclampx,
        alpha: GLclampx,
    ) {
        self.ClearColor(
            fixed_to_float(red),
            fixed_to_float(green),
            fixed_to_float(blue),
            fixed_to_float(alpha),
        )
    }
    unsafe fn ClearDepthf(&mut self, depth: GLclampf) {
        self.state.clear_depth = depth;
        gl3::ClearDepthf(depth)
    }
    unsafe fn ClearDepthx(&mut self, depth: GLclampx) {
        self.ClearDepthf(fixed_to_float(depth))
    }
    unsafe fn ClearStencil(&mut self, s: GLint) {
        self.state.clear_stencil = s;
        gl3::ClearStencil(s)
    }

    // Textures
    unsafe fn PixelStorei(&mut self, pname: GLenum, param: GLint) {
        match pname {
            gles11::PACK_ALIGNMENT => self.state.pack_alignment = param,
            gles11::UNPACK_ALIGNMENT => self.state.unpack_alignment = param,
            _ => panic!("Unhandled glPixelStorei({pname:#x})"),
        }
        gl3::PixelStorei(pname, param)
    }
    unsafe fn ReadPixels(
        &mut self,
        x: GLint,
        y: GLint,
        width: GLsizei,
        height: GLsizei,
        format: GLenum,
        type_: GLenum,
        pixels: *mut GLvoid,
    ) {
        gl3::ReadPixels(x, y, width, height, format, type_, pixels)
    }
    unsafe fn GenTextures(&mut self, n: GLsizei, textures: *mut GLuint) {
        gl3::GenTextures(n, textures)
    }
    unsafe fn DeleteTextures(&mut self, n: GLsizei, textures: *const GLuint) {
        for i in 0..n as usize {
            let texture = *textures.add(i);
            with_shared(|s| {
                s.texture_formats.remove(&texture);
                s.generate_mipmap.remove(&texture);
            });
            for unit in self.state.units.iter_mut() {
                if unit.binding == texture {
                    unit.binding = 0;
                }
            }
        }
        gl3::DeleteTextures(n, textures)
    }
    unsafe fn ActiveTexture(&mut self, texture: GLenum) {
        let unit = texture.wrapping_sub(gles11::TEXTURE0) as usize;
        assert!(unit < TEXTURE_UNITS, "Bad glActiveTexture({texture:#x})");
        self.state.active_texture = unit;
        gl3::ActiveTexture(texture)
    }
    unsafe fn IsTexture(&mut self, texture: GLuint) -> GLboolean {
        gl3::IsTexture(texture)
    }
    unsafe fn BindTexture(&mut self, target: GLenum, texture: GLuint) {
        assert_eq!(target, gles11::TEXTURE_2D);
        self.state.units[self.state.active_texture].binding = texture;
        gl3::BindTexture(target, texture)
    }
    unsafe fn TexParameteri(&mut self, target: GLenum, pname: GLenum, param: GLint) {
        assert_eq!(target, gles11::TEXTURE_2D);
        match pname {
            gles11::GENERATE_MIPMAP => {
                let binding = self.state.units[self.state.active_texture].binding;
                with_shared(|s| s.generate_mipmap.insert(binding, param != 0));
            }
            gles11::TEXTURE_MAX_ANISOTROPY_EXT | gles11::TEXTURE_LOD_BIAS_EXT => {
                log_dbg!("Ignoring glTexParameter({:#x})", pname);
            }
            _ => gl3::TexParameteri(target, pname, param),
        }
    }
    unsafe fn TexParameterf(&mut self, target: GLenum, pname: GLenum, param: GLfloat) {
        match pname {
            gles11::TEXTURE_MAX_ANISOTROPY_EXT | gles11::TEXTURE_LOD_BIAS_EXT => {
                log_dbg!("Ignoring glTexParameter({:#x})", pname);
            }
            _ => self.TexParameteri(target, pname, param as GLint),
        }
    }
    unsafe fn TexParameterx(&mut self, target: GLenum, pname: GLenum, param: GLfixed) {
        // Enum-valued parameters are passed as-is, not as fixed-point.
        match pname {
            gles11::TEXTURE_MAX_ANISOTROPY_EXT | gles11::TEXTURE_LOD_BIAS_EXT => {
                self.TexParameterf(target, pname, fixed_to_float(param))
            }
            _ => self.TexParameteri(target, pname, param),
        }
    }
    unsafe fn TexParameteriv(&mut self, target: GLenum, pname: GLenum, params: *const GLint) {
        self.TexParameteri(target, pname, *params)
    }
    unsafe fn TexParameterfv(&mut self, target: GLenum, pname: GLenum, params: *const GLfloat) {
        self.TexParameterf(target, pname, *params)
    }
    unsafe fn TexParameterxv(&mut self, target: GLenum, pname: GLenum, params: *const GLfixed) {
        self.TexParameterx(target, pname, *params)
    }
    unsafe fn TexImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        internalformat: GLint,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
        format: GLenum,
        type_: GLenum,
        pixels: *const GLvoid,
    ) {
        assert_eq!(target, gles11::TEXTURE_2D);
        let converted = self.convert_pixels(width, height, format, type_, pixels);
        let (format, pixels) = match &converted {
            Some((format, data)) => (*format, data.as_ptr().cast()),
            None if format == gles11::BGRA_EXT => (gles11::RGBA, pixels),
            None => (format, pixels),
        };
        // OpenGL ES 1.1 requires internalformat to match format, except that
        // BGRA data is stored as RGBA. OpenGL ES 3.0 requires them to match.
        let _ = internalformat;
        let internalformat = format;
        if level == 0 {
            let binding = self.state.units[self.state.active_texture].binding;
            with_shared(|s| s.texture_formats.insert(binding, internalformat));
        }
        if converted.is_some() {
            gl3::PixelStorei(gl3::UNPACK_ALIGNMENT, 1);
        }
        gl3::TexImage2D(
            target,
            level,
            internalformat as GLint,
            width,
            height,
            border,
            format,
            type_,
            pixels,
        );
        if converted.is_some() {
            gl3::PixelStorei(gl3::UNPACK_ALIGNMENT, self.state.unpack_alignment);
        }
        self.maybe_generate_mipmap(level);
    }
    unsafe fn TexSubImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        xoffset: GLint,
        yoffset: GLint,
        width: GLsizei,
        height: GLsizei,
        format: GLenum,
        type_: GLenum,
        pixels: *const GLvoid,
    ) {
        assert_eq!(target, gles11::TEXTURE_2D);
        let converted = self.convert_pixels(width, height, format, type_, pixels);
        let (format, pixels) = match &converted {
            Some((format, data)) => (*format, data.as_ptr().cast()),
            None => (format, pixels),
        };
        if converted.is_some() {
            gl3::PixelStorei(gl3::UNPACK_ALIGNMENT, 1);
        }
        gl3::TexSubImage2D(
            target, level, xoffset, yoffset, width, height, format, type_, pixels,
        );
        if converted.is_some() {
            gl3::PixelStorei(gl3::UNPACK_ALIGNMENT, self.state.unpack_alignment);
        }
        self.maybe_generate_mipmap(level);
    }
    unsafe fn CompressedTexImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        internalformat: GLenum,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
        image_size: GLsizei,
        data: *const GLvoid,
    ) {
        let data = std::slice::from_raw_parts(data.cast::<u8>(), image_size as usize);
        if try_decode_pvrtc(
            self,
            target,
            level,
            internalformat,
            width,
            height,
            border,
            data,
        ) {
            log_dbg!("Decoded PVRTC");
            return;
        }
        let Some(PalettedTextureFormat {
            index_is_nibble,
            palette_entry_format,
            palette_entry_type,
        }) = PalettedTextureFormat::get_info(internalformat)
        else {
            unimplemented!("CompressedTexImage2D internalformat: {:#x}", internalformat);
        };
        let palette_entry_size = match (palette_entry_type, palette_entry_format) {
            (gles11::UNSIGNED_BYTE, gles11::RGB) => 3,
            (gles11::UNSIGNED_BYTE, gles11::RGBA) => 4,
            _ => 2,
        };
        let palette_size = palette_entry_size * if index_is_nibble { 16 } else { 256 };
        let index_count = width as usize * height as usize;
        // TODO: support multiple miplevels in one image
        assert!(level == 0);
        let (palette, indices) = data.split_at(palette_size);
        let mut decoded = Vec::<u8>::with_capacity(palette_entry_size * index_count);
        for i in 0..index_count {
            let index = if index_is_nibble {
                (indices[i / 2] >> ((1 - (i % 2)) * 4)) & 0xf
            } else {
                indices[i]
            } as usize;
            decoded.extend_from_slice(&palette[index * palette_entry_size..][..palette_entry_size]);
        }
        gl3::PixelStorei(gl3::UNPACK_ALIGNMENT, 1);
        self.TexImage2D(
            target,
            level,
            palette_entry_format as _,
            width,
            height,
            border,
            palette_entry_format,
            palette_entry_type,
            decoded.as_ptr().cast(),
        );
        gl3::PixelStorei(gl3::UNPACK_ALIGNMENT, self.state.unpack_alignment);
    }
    unsafe fn CopyTexImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        internalformat: GLenum,
        x: GLint,
        y: GLint,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
    ) {
        if level == 0 {
            let binding = self.state.units[self.state.active_texture].binding;
            with_shared(|s| s.texture_formats.insert(binding, internalformat));
        }
        gl3::CopyTexImage2D(target, level, internalformat, x, y, width, height, border);
        self.maybe_generate_mipmap(level);
    }
    unsafe fn CopyTexSubImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        xoffset: GLint,
        yoffset: GLint,
        x: GLint,
        y: GLint,
        width: GLsizei,
        height: GLsizei,
    ) {
        gl3::CopyTexSubImage2D(target, level, xoffset, yoffset, x, y, width, height);
        self.maybe_generate_mipmap(level);
    }
    unsafe fn TexEnvf(&mut self, target: GLenum, pname: GLenum, param: GLfloat) {
        match pname {
            gles11::RGB_SCALE | gles11::ALPHA_SCALE => {
                assert_eq!(target, gles11::TEXTURE_ENV);
                let unit = &mut self.state.units[self.state.active_texture];
                if pname == gles11::RGB_SCALE {
                    unit.rgb_scale = param;
                } else {
                    unit.alpha_scale = param;
                }
            }
            _ => self.TexEnvi(target, pname, param as GLint),
        }
    }
    unsafe fn TexEnvx(&mut self, target: GLenum, pname: GLenum, param: GLfixed) {
        match pname {
            gles11::RGB_SCALE | gles11::ALPHA_SCALE => {
                self.TexEnvf(target, pname, fixed_to_float(param))
            }
            // Enum values are passed as-is, not as fixed-point.
            _ => self.TexEnvi(target, pname, param),
        }
    }
    unsafe fn TexEnvi(&mut self, target: GLenum, pname: GLenum, param: GLint) {
        if target == gles11::TEXTURE_FILTER_CONTROL_EXT {
            log_dbg!("Ignoring glTexEnv(TEXTURE_FILTER_CONTROL_EXT, {:#x})", pname);
            return;
        }
        let unit = &mut self.state.units[self.state.active_texture];
        if target == oes::POINT_SPRITE_OES {
            assert_eq!(pname, oes::COORD_REPLACE_OES);
            unit.coord_replace = param != 0;
            return;
        }
        assert_eq!(target, gles11::TEXTURE_ENV);
        let param = param as GLenum;
        let env = &mut unit.env;
        match pname {
            gles11::TEXTURE_ENV_MODE => env.mode = param,
            gles11::COMBINE_RGB => env.combine_rgb = param,
            gles11::COMBINE_ALPHA => env.combine_alpha = param,
            gles11::SRC0_RGB => env.src_rgb[0] = param,
            gles11::SRC1_RGB => env.src_rgb[1] = param,
            gles11::SRC2_RGB => env.src_rgb[2] = param,
            gles11::SRC0_ALPHA => env.src_alpha[0] = param,
            gles11::SRC1_ALPHA => env.src_alpha[1] = param,
            gles11::SRC2_ALPHA => env.src_alpha[2] = param,
            gles11::OPERAND0_RGB => env.operand_rgb[0] = param,
            gles11::OPERAND1_RGB => env.operand_rgb[1] = param,
            gles11::OPERAND2_RGB => env.operand_rgb[2] = param,
            gles11::OPERAND0_ALPHA => env.operand_alpha[0] = param,
            gles11::OPERAND1_ALPHA => env.operand_alpha[1] = param,
            gles11::OPERAND2_ALPHA => env.operand_alpha[2] = param,
            gles11::RGB_SCALE => unit.rgb_scale = param as f32,
            gles11::ALPHA_SCALE => unit.alpha_scale = param as f32,
            oes::COORD_REPLACE_OES => unit.coord_replace = param != 0,
            _ => panic!("Unhandled glTexEnv({pname:#x})"),
        }
    }
    unsafe fn TexEnvfv(&mut self, target: GLenum, pname: GLenum, params: *const GLfloat) {
        if pname == gles11::TEXTURE_ENV_COLOR {
            assert_eq!(target, gles11::TEXTURE_ENV);
            self.state.units[self.state.active_texture].env_color =
                std::array::from_fn(|i| *params.add(i));
        } else {
            self.TexEnvf(target, pname, *params)
        }
    }
    unsafe fn TexEnvxv(&mut self, target: GLenum, pname: GLenum, params: *const GLfixed) {
        if pname == gles11::TEXTURE_ENV_COLOR {
            let color: [f32; 4] = std::array::from_fn(|i| fixed_to_float(*params.add(i)));
            self.TexEnvfv(target, pname, color.as_ptr())
        } else {
            self.TexEnvx(target, pname, *params)
        }
    }
    unsafe fn TexEnviv(&mut self, target: GLenum, pname: GLenum, params: *const GLint) {
        if pname == gles11::TEXTURE_ENV_COLOR {
            // Integer colors map the full integer range to [-1, 1].
            let color: [f32; 4] =
                std::array::from_fn(|i| (*params.add(i) as f64 / GLint::MAX as f64) as f32);
            self.TexEnvfv(target, pname, color.as_ptr())
        } else {
            self.TexEnvi(target, pname, *params)
        }
    }
    unsafe fn MultiTexCoord4f(
        &mut self,
        target: GLenum,
        s: GLfloat,
        t: GLfloat,
        r: GLfloat,
        q: GLfloat,
    ) {
        let unit = target.wrapping_sub(gles11::TEXTURE0) as usize;
        assert!(unit < TEXTURE_UNITS);
        self.state.units[unit].current_texcoord = [s, t, r, q];
    }
    unsafe fn MultiTexCoord4x(
        &mut self,
        target: GLenum,
        s: GLfixed,
        t: GLfixed,
        r: GLfixed,
        q: GLfixed,
    ) {
        self.MultiTexCoord4f(
            target,
            fixed_to_float(s),
            fixed_to_float(t),
            fixed_to_float(r),
            fixed_to_float(q),
        )
    }

    // Matrix stack operations
    unsafe fn MatrixMode(&mut self, mode: GLenum) {
        assert!([gles11::MODELVIEW, gles11::PROJECTION, gles11::TEXTURE].contains(&mode));
        self.state.matrix_mode = mode;
    }
    unsafe fn LoadIdentity(&mut self) {
        *self.state.current_matrix() = IDENTITY;
    }
    unsafe fn LoadMatrixf(&mut self, m: *const GLfloat) {
        *self.state.current_matrix() = std::array::from_fn(|i| m.add(i).read_unaligned());
    }
    unsafe fn LoadMatrixx(&mut self, m: *const GLfixed) {
        *self.state.current_matrix() = matrix_fixed_to_float(m);
    }
    unsafe fn MultMatrixf(&mut self, m: *const GLfloat) {
        let m: Mat4 = std::array::from_fn(|i| m.add(i).read_unaligned());
        self.mult(&m);
    }
    unsafe fn MultMatrixx(&mut self, m: *const GLfixed) {
        let m = matrix_fixed_to_float(m);
        self.mult(&m);
    }
    unsafe fn PushMatrix(&mut self) {
        let max = match self.state.matrix_mode {
            gles11::MODELVIEW => MODELVIEW_STACK_DEPTH,
            gles11::PROJECTION => PROJECTION_STACK_DEPTH,
            _ => TEXTURE_STACK_DEPTH,
        };
        let stack = self.state.current_matrix_stack();
        if stack.len() >= max {
            self.state.set_error(gles11::STACK_OVERFLOW);
            return;
        }
        let top = *stack.last().unwrap();
        stack.push(top);
    }
    unsafe fn PopMatrix(&mut self) {
        let stack = self.state.current_matrix_stack();
        if stack.len() <= 1 {
            self.state.set_error(gles11::STACK_UNDERFLOW);
            return;
        }
        stack.pop();
    }
    unsafe fn Orthof(
        &mut self,
        left: GLfloat,
        right: GLfloat,
        bottom: GLfloat,
        top: GLfloat,
        near: GLfloat,
        far: GLfloat,
    ) {
        self.mult(&mat_ortho(left, right, bottom, top, near, far));
    }
    unsafe fn Orthox(
        &mut self,
        left: GLfixed,
        right: GLfixed,
        bottom: GLfixed,
        top: GLfixed,
        near: GLfixed,
        far: GLfixed,
    ) {
        self.Orthof(
            fixed_to_float(left),
            fixed_to_float(right),
            fixed_to_float(bottom),
            fixed_to_float(top),
            fixed_to_float(near),
            fixed_to_float(far),
        )
    }
    unsafe fn Frustumf(
        &mut self,
        left: GLfloat,
        right: GLfloat,
        bottom: GLfloat,
        top: GLfloat,
        near: GLfloat,
        far: GLfloat,
    ) {
        self.mult(&mat_frustum(left, right, bottom, top, near, far));
    }
    unsafe fn Frustumx(
        &mut self,
        left: GLfixed,
        right: GLfixed,
        bottom: GLfixed,
        top: GLfixed,
        near: GLfixed,
        far: GLfixed,
    ) {
        self.Frustumf(
            fixed_to_float(left),
            fixed_to_float(right),
            fixed_to_float(bottom),
            fixed_to_float(top),
            fixed_to_float(near),
            fixed_to_float(far),
        )
    }
    unsafe fn Rotatef(&mut self, angle: GLfloat, x: GLfloat, y: GLfloat, z: GLfloat) {
        self.mult(&mat_rotate(angle, x, y, z));
    }
    unsafe fn Rotatex(&mut self, angle: GLfixed, x: GLfixed, y: GLfixed, z: GLfixed) {
        self.Rotatef(
            fixed_to_float(angle),
            fixed_to_float(x),
            fixed_to_float(y),
            fixed_to_float(z),
        )
    }
    unsafe fn Scalef(&mut self, x: GLfloat, y: GLfloat, z: GLfloat) {
        let mut m = IDENTITY;
        m[0] = x;
        m[5] = y;
        m[10] = z;
        self.mult(&m);
    }
    unsafe fn Scalex(&mut self, x: GLfixed, y: GLfixed, z: GLfixed) {
        self.Scalef(fixed_to_float(x), fixed_to_float(y), fixed_to_float(z))
    }
    unsafe fn Translatef(&mut self, x: GLfloat, y: GLfloat, z: GLfloat) {
        let mut m = IDENTITY;
        m[12] = x;
        m[13] = y;
        m[14] = z;
        self.mult(&m);
    }
    unsafe fn Translatex(&mut self, x: GLfixed, y: GLfixed, z: GLfixed) {
        self.Translatef(fixed_to_float(x), fixed_to_float(y), fixed_to_float(z))
    }

    // OES_framebuffer_object -> OpenGL ES 3.0 core
    unsafe fn GenFramebuffersOES(&mut self, n: GLsizei, framebuffers: *mut GLuint) {
        gl3::GenFramebuffers(n, framebuffers)
    }
    unsafe fn GenRenderbuffersOES(&mut self, n: GLsizei, renderbuffers: *mut GLuint) {
        gl3::GenRenderbuffers(n, renderbuffers)
    }
    unsafe fn IsFramebufferOES(&mut self, framebuffer: GLuint) -> GLboolean {
        gl3::IsFramebuffer(framebuffer)
    }
    unsafe fn IsRenderbufferOES(&mut self, renderbuffer: GLuint) -> GLboolean {
        gl3::IsRenderbuffer(renderbuffer)
    }
    unsafe fn BindFramebufferOES(&mut self, target: GLenum, framebuffer: GLuint) {
        self.state.framebuffer = framebuffer;
        gl3::BindFramebuffer(target, framebuffer)
    }
    unsafe fn BindRenderbufferOES(&mut self, target: GLenum, renderbuffer: GLuint) {
        self.state.renderbuffer = renderbuffer;
        gl3::BindRenderbuffer(target, renderbuffer)
    }
    unsafe fn RenderbufferStorageOES(
        &mut self,
        target: GLenum,
        internalformat: GLenum,
        width: GLsizei,
        height: GLsizei,
    ) {
        // OES_rgb8_rgba8 and OES_depth24 formats have the same values as
        // their OpenGL ES 3.0 equivalents.
        gl3::RenderbufferStorage(target, internalformat, width, height)
    }
    unsafe fn FramebufferRenderbufferOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        renderbuffertarget: GLenum,
        renderbuffer: GLuint,
    ) {
        gl3::FramebufferRenderbuffer(target, attachment, renderbuffertarget, renderbuffer)
    }
    unsafe fn FramebufferTexture2DOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        textarget: GLenum,
        texture: GLuint,
        level: i32,
    ) {
        gl3::FramebufferTexture2D(target, attachment, textarget, texture, level)
    }
    unsafe fn GetFramebufferAttachmentParameterivOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        pname: GLenum,
        params: *mut GLint,
    ) {
        gl3::GetFramebufferAttachmentParameteriv(target, attachment, pname, params)
    }
    unsafe fn GetRenderbufferParameterivOES(
        &mut self,
        target: GLenum,
        pname: GLenum,
        params: *mut GLint,
    ) {
        gl3::GetRenderbufferParameteriv(target, pname, params)
    }
    unsafe fn CheckFramebufferStatusOES(&mut self, target: GLenum) -> GLenum {
        gl3::CheckFramebufferStatus(target)
    }
    unsafe fn DeleteFramebuffersOES(&mut self, n: GLsizei, framebuffers: *const GLuint) {
        for i in 0..n as usize {
            if *framebuffers.add(i) == self.state.framebuffer {
                self.state.framebuffer = 0;
            }
        }
        gl3::DeleteFramebuffers(n, framebuffers)
    }
    unsafe fn DeleteRenderbuffersOES(&mut self, n: GLsizei, renderbuffers: *const GLuint) {
        for i in 0..n as usize {
            if *renderbuffers.add(i) == self.state.renderbuffer {
                self.state.renderbuffer = 0;
            }
        }
        gl3::DeleteRenderbuffers(n, renderbuffers)
    }
    unsafe fn GenerateMipmapOES(&mut self, target: GLenum) {
        gl3::GenerateMipmap(target)
    }
    unsafe fn GetBufferParameteriv(&mut self, target: GLenum, pname: GLenum, params: *mut GLint) {
        gl3::GetBufferParameteriv(target, pname, params)
    }
    unsafe fn MapBufferOES(&mut self, target: GLenum, access: GLenum) -> *mut GLvoid {
        assert_eq!(access, gles11::WRITE_ONLY_OES);
        let buffer = self.buffer_for_target(target);
        with_shared(|s| {
            assert!(s.mapped.is_none(), "Only one mapped buffer is supported");
            s.mapped = Some(buffer);
            s.buffer_data
                .get_mut(&buffer)
                .expect("Mapped an empty buffer")
                .as_mut_ptr()
                .cast()
        })
    }
    unsafe fn UnmapBufferOES(&mut self, target: GLenum) -> GLboolean {
        let buffer = self.buffer_for_target(target);
        with_shared(|s| {
            assert_eq!(s.mapped.take(), Some(buffer));
            let data = &s.buffer_data[&buffer];
            gl3::BufferSubData(target, 0, data.len() as GLsizeiptr, data.as_ptr().cast());
        });
        gles11::TRUE
    }
}

impl GLES1OnGLES3<'_> {
    fn mult(&mut self, m: &Mat4) {
        let current = self.state.current_matrix();
        *current = mat_mul(current, m);
    }

    unsafe fn set_pointer(
        &mut self,
        name: GLenum,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        let buffer = self.state.array_buffer;
        let array = self.state.array_mut(name).unwrap();
        array.size = size;
        array.type_ = type_;
        array.stride = stride;
        array.pointer = pointer;
        array.buffer = buffer;
    }
}
