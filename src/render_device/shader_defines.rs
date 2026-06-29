//! Maps a [`Shader`] variant to the WGSL preprocessor defines that select the
//! matching shader in `noesis.wgsl`.

use std::collections::HashSet;

use noesis_runtime::render_device::types::Shader;

/// Returns the WGSL define set for `shader`.
///
/// # Panics
///
/// Panics if `shader` has no `noesis.wgsl` variant yet; the message names the
/// missing one.
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

        // PAINT_PATTERN_PLAIN gates the no-wrap branch in noesis.wgsl; each
        // CLAMP / REPEAT / MIRROR_{U,V} / MIRROR variant below gates its own
        // block instead.
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

        // ─── Pattern wrap variants ─────────────────────────────────────────
        // Vertex format differences (all mirror the SDK's FORMAT_FOR_VERTEX):
        //   CLAMP         → PosTex0Rect     (pos + uv0 + rect)
        //   REPEAT+MIRROR → PosTex0RectTile (pos + uv0 + rect + tile)
        // The AA twins add coverage. `HAS_RECT` / `HAS_TILE` gate the
        // matching VsIn attribute declarations.
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

        // PAINT_LINEAR samples the `ramps` gradient texture.
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

        // PAINT_RADIAL samples `ramps` at a computed radius.
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

        // SDF text. Vertex format PosColorTex1: pos (loc 0), color (loc 1), uv1 (loc 3).
        // The "paint texture" at group(2) carries the glyph atlas rather than
        // a pattern/ramp; the Rust side picks the right batch slot to bind.
        n if n == Shader::SDF_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_ST1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_SDF");
        }

        // Subpixel text. Same vertex format as SDF_SOLID (PosColorTex1) and the same glyph
        // atlas at group(2); the difference is a dual-source fragment output
        // (`@blend_src(0)` / `@blend_src(1)`) carrying per-channel subpixel
        // coverage, composited with the `SrcOver_Dual` blend mode. Requires
        // the device's `DUAL_SOURCE_BLENDING` feature; Noesis only emits these
        // when `DeviceCaps::subpixel_rendering` is set (see `caps()` notes).
        n if n == Shader::SDF_LCD_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_UV1");
            d.insert("HAS_ST1");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_SDF_LCD");
        }

        // GL ref Shader.140.frag EFFECT_OPACITY block:
        //   fragColor = texture(image, uv1) * (opacity_ * paint.a)
        // `image` is the offscreen-rendered pass of the layer being
        // composited; the paint side controls the per-pixel opacity
        // multiplier via its alpha (and the global `opacity` scalar via
        // its uniform). HAS_IMAGE_TEXTURE pulls in the second
        // texture+sampler pair the WGSL declares at group(3); HAS_UV1
        // carries the sample coords for that texture (location 3).
        //
        // Noesis emits these when a layer composites back through an Opacity
        // animation, an opacity mask, or a focus-visual fade.
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

        // GL ref FSHADER(DOWNSAMPLE) / FSHADER(UPSAMPLE), no PAINT. These
        // form the separable-blur resolve chain Noesis runs in the offscreen
        // phase. DOWNSAMPLE box-filters four taps of `pattern` (group 2) at
        // VS-computed UVs (vertex shader sets the DOWNSAMPLE flag to spread
        // uv0 ± uv1 into uv0..uv3). UPSAMPLE blends the lower-res `image`
        // (group 3) with the same-res `pattern` (group 2) by `color.a`.
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

        // Drop shadow. GL ref FSHADER2(SHADOW, SOLID). Vertex format PosColorTex1Rect:
        // pos, color, uv1 (layer sample coords), rect (clamp bounds). Reads
        // both the layer `image` (group 3 binding 0/1) and the blurred
        // `shadow` (group 3 binding 2/3), plus cbuffer1_ps (group 1 binding 1)
        // for shadow color / offset / blend factor.
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

        // Gaussian blur resolve. GL ref FSHADER2(BLUR, SOLID). Vertex format PosColorTex1:
        // pos, color, uv1. Crossfades the layer `image` with the blurred
        // `shadow` by cbuffer1_ps[0].
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
