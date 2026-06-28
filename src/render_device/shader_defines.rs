//! Maps `Noesis::Shader::Enum` values to the WGSL preprocessor define set
//! that produces the right shader variant. Mirrors the `Shader.140.frag` /
//! `Shader.140.vert` `#define` cascade in the `GLRenderDevice` reference.
//!
//! Phase 3.A coverage: only [`Shader::PATH_SOLID`]. Phase 3.B fills in
//! `Path_AA_Solid`, `Mask`, `RGBA`, `Clear`. Phase 4.B.2 adds the plain
//! [`Shader::PATH_PATTERN`] + [`Shader::PATH_AA_PATTERN`] variants (no
//! explicit wrap / clamp). Linear/Radial/SDF/Opacity/wrap variants still
//! need additional resource bindings and land later.

use std::collections::HashSet;

use dm_noesis_runtime::render_device::types::Shader;

/// Returns the define set for `shader`.
///
/// # Panics
///
/// Panics if `shader` hasn't been ported to `noesis.wgsl` yet — the panic
/// names the missing variant so it doubles as a TODO list.
#[must_use]
#[allow(clippy::too_many_lines)] // one arm per shader variant, no abstraction buys clarity here
pub fn defines_for_shader(shader: Shader) -> HashSet<&'static str> {
    let mut d: HashSet<&'static str> = HashSet::new();
    match shader.0 {
        // ─── Effect-only (no paint) ────────────────────────────────────────
        n if n == Shader::RGBA.0 => {
            d.insert("EFFECT_RGBA");
        }
        n if n == Shader::MASK.0 => {
            d.insert("EFFECT_MASK");
        }
        n if n == Shader::CLEAR.0 => {
            d.insert("EFFECT_CLEAR");
        }

        // ─── EFFECT_PATH × PAINT_SOLID ─────────────────────────────────────
        n if n == Shader::PATH_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_PATH");
        }

        // ─── EFFECT_PATH_AA × PAINT_SOLID ──────────────────────────────────
        n if n == Shader::PATH_AA_SOLID.0 => {
            d.insert("HAS_COLOR");
            d.insert("HAS_COVERAGE");
            d.insert("PAINT_SOLID");
            d.insert("EFFECT_PATH_AA");
        }

        // ─── EFFECT_PATH × PAINT_PATTERN (plain — sampler wrap does it) ────
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

        // ─── EFFECT_PATH_AA × PAINT_PATTERN (plain) ────────────────────────
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

        // ─── EFFECT_PATH × PAINT_LINEAR (samples `ramps` texture) ──────────
        n if n == Shader::PATH_LINEAR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_LINEAR");
            d.insert("EFFECT_PATH");
        }

        // ─── EFFECT_PATH_AA × PAINT_LINEAR ─────────────────────────────────
        n if n == Shader::PATH_AA_LINEAR.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_LINEAR");
            d.insert("EFFECT_PATH_AA");
        }

        // ─── EFFECT_PATH × PAINT_RADIAL (samples `ramps` at computed radius)
        n if n == Shader::PATH_RADIAL.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_RADIAL");
            d.insert("EFFECT_PATH");
        }

        // ─── EFFECT_PATH_AA × PAINT_RADIAL ─────────────────────────────────
        n if n == Shader::PATH_AA_RADIAL.0 => {
            d.insert("HAS_UV0");
            d.insert("HAS_COVERAGE");
            d.insert("HAS_PAINT_TEXTURE");
            d.insert("PAINT_RADIAL");
            d.insert("EFFECT_PATH_AA");
        }

        // ─── EFFECT_SDF × PAINT_SOLID (text) ───────────────────────────────
        // Vertex format PosColorTex1: pos (loc 0), color (loc 1), uv1 (loc 3).
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

        // ─── EFFECT_OPACITY × paint variants ───────────────────────────────
        // GL ref Shader.140.frag EFFECT_OPACITY block:
        //   fragColor = texture(image, uv1) * (opacity_ * paint.a)
        // `image` is the offscreen-rendered pass of the layer being
        // composited; the paint side controls the per-pixel opacity
        // multiplier via its alpha (and the global `opacity` scalar via
        // its uniform). HAS_IMAGE_TEXTURE pulls in the second
        // texture+sampler pair the WGSL declares at group(3); HAS_UV1
        // carries the sample coords for that texture (location 3).
        //
        // Triggered by Noesis whenever a layer needs to composite back
        // through an Opacity property animation, an opacity mask, or
        // a focus-visual fade — the dev console hits this on the second
        // open after a focused TextBox's caret blink animation kicks
        // in (CaretBrush opacity 1 → 0 every blink interval).
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

        other => panic!(
            "Shader({other}) not yet ported to noesis.wgsl. \
             Phase 3 is rolling out variants incrementally — extend \
             shader_defines::defines_for_shader and noesis.wgsl together."
        ),
    }
    d
}
