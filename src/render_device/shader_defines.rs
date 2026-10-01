//! The WGSL define set for each Noesis [`Shader`], which selects that shader's
//! variant of `noesis.wgsl` through [`preprocess`].
//!
//! [`preprocess`]: crate::render_device::shader_preproc::preprocess

use std::collections::HashSet;

use noesis_runtime::render_device::types::Shader;

/// Returns the WGSL define set for `shader`.
///
/// # Panics
///
/// Panics if `shader` has no `noesis.wgsl` variant: the `SDF_*` gradient and
/// pattern paints, every `SDF_LCD_*` except `SDF_LCD_SOLID`, and
/// `CUSTOM_EFFECT`.
#[must_use]
#[allow(clippy::too_many_lines)] // one arm per shader variant, no abstraction buys clarity here
pub fn defines_for_shader(shader: Shader) -> HashSet<&'static str> {
    let mut d: HashSet<&'static str> = HashSet::new();
    match shader.0 {
        n if n == Shader::RGBA.0 => {
            d.insert("EFFECT_RGBA");
        }
        n if n == Shader::MASK.0 => {
            d.insert("EFFECT_MASK");
        }
        n if n == Shader::CLEAR.0 => {
            d.insert("EFFECT_CLEAR");
        }

        n if n == Shader::PATH_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_PATH");
        }

        n if n == Shader::PATH_AA_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_COVERAGE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_PATH_AA");
        }

        // PAINT_PATTERN_PLAIN gates the no-wrap branch; the wrap variants
        // below gate their own blocks instead.
        n if n == Shader::PATH_PATTERN.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("PAINT_PATTERN_PLAIN");
            d.insert("EFFECT_PATH");
        }

        n if n == Shader::PATH_AA_PATTERN.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("PAINT_PATTERN_PLAIN");
            d.insert("EFFECT_PATH_AA");
        }

        // HAS_RECT / HAS_TILE must match the SDK's FORMAT_FOR_VERTEX: CLAMP is
        // PosTex0Rect, REPEAT and MIRROR* are PosTex0RectTile, AA adds coverage.
        n if n == Shader::PATH_PATTERN_CLAMP.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_RECT");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("CLAMP_PATTERN");
            d.insert("EFFECT_PATH");
        }
        n if n == Shader::PATH_AA_PATTERN_CLAMP.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_RECT");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("CLAMP_PATTERN");
            d.insert("EFFECT_PATH_AA");
        }
        n if n == Shader::PATH_PATTERN_REPEAT.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("REPEAT_PATTERN");
            d.insert("EFFECT_PATH");
        }
        n if n == Shader::PATH_AA_PATTERN_REPEAT.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("REPEAT_PATTERN");
            d.insert("EFFECT_PATH_AA");
        }
        n if n == Shader::PATH_PATTERN_MIRROR_U.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRRORU_PATTERN");
            d.insert("EFFECT_PATH");
        }
        n if n == Shader::PATH_AA_PATTERN_MIRROR_U.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRRORU_PATTERN");
            d.insert("EFFECT_PATH_AA");
        }
        n if n == Shader::PATH_PATTERN_MIRROR_V.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRRORV_PATTERN");
            d.insert("EFFECT_PATH");
        }
        n if n == Shader::PATH_AA_PATTERN_MIRROR_V.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRRORV_PATTERN");
            d.insert("EFFECT_PATH_AA");
        }
        n if n == Shader::PATH_PATTERN_MIRROR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRROR_PATTERN");
            d.insert("EFFECT_PATH");
        }
        n if n == Shader::PATH_AA_PATTERN_MIRROR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_PATTERN");
            d.insert("MIRROR_PATTERN");
            d.insert("EFFECT_PATH_AA");
        }

        n if n == Shader::PATH_LINEAR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_LINEAR");
            d.insert("EFFECT_PATH");
        }

        n if n == Shader::PATH_AA_LINEAR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_LINEAR");
            d.insert("EFFECT_PATH_AA");
        }

        n if n == Shader::PATH_RADIAL.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_RADIAL");
            d.insert("EFFECT_PATH");
        }

        n if n == Shader::PATH_AA_RADIAL.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_RADIAL");
            d.insert("EFFECT_PATH_AA");
        }

        // group(2) carries the glyph atlas here, not a pattern or ramp.
        n if n == Shader::SDF_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_ST1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_SDF");
        }

        // Needs wgpu's DUAL_SOURCE_BLENDING; Noesis only emits it when
        // `DeviceCaps::subpixel_rendering` is set, which the device leaves off.
        n if n == Shader::SDF_LCD_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_ST1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_SDF_LCD");
        }

        n if n == Shader::OPACITY_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_LINEAR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_LINEAR");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_RADIAL.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_RADIAL");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("PAINT_PATTERN_PLAIN");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN_CLAMP.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("CLAMP_PATTERN");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN_REPEAT.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("REPEAT_PATTERN");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN_MIRROR_U.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("MIRRORU_PATTERN");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN_MIRROR_V.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("MIRRORV_PATTERN");
            d.insert("EFFECT_OPACITY");
        }
        n if n == Shader::OPACITY_PATTERN_MIRROR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_TILE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("MIRROR_PATTERN");
            d.insert("EFFECT_OPACITY");
        }

        // No PAINT. The DOWNSAMPLE define (distinct from EFFECT_DOWNSAMPLE)
        // makes the vertex shader spread uv0 +/- uv1 into four tap coords.
        n if n == Shader::DOWNSAMPLE.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("DOWNSAMPLE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("EFFECT_DOWNSAMPLE");
        }
        n if n == Shader::UPSAMPLE.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV0");
            d.insert("HAS_UV1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("EFFECT_UPSAMPLE");
        }

        // Reads `image` and `shadow` at group(3) plus cbuffer1_ps for the shadow
        // color, offset, and blend factor.
        n if n == Shader::SHADOW.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_RECT");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("HAS_SHADOW_TEXTURE");
            d.insert("HAS_CBUFFER1_PS");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_SHADOW");
        }

        n if n == Shader::BLUR.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_IMAGE_TEXTURE");
            d.insert("HAS_SHADOW_TEXTURE");
            d.insert("HAS_CBUFFER1_PS");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_BLUR");
        }

        other => panic!(
            "Shader({other}) not yet ported to noesis.wgsl. \
             CUSTOM_EFFECT (52) needs user pixel-shader compilation via \
             Batch.pixelShader. Extend \
             shader_defines::defines_for_shader and noesis.wgsl together."
        ),
    }
    d
}
