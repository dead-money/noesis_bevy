// SDF_LCD_SOLID emits a dual-source fragment output (`@blend_src`), which WGSL
// gates behind an enable directive. It must precede all other declarations, so
// it lives at the very top — emitted only for the LCD variant so the other
// shaders don't require the device's DUAL_SOURCE_BLENDING feature.
#ifdef EFFECT_SDF_LCD
enable dual_source_blending;
#endif

// Unified shader source for the Noesis pipeline matrix. Variants are produced
// at pipeline-build time by stripping #ifdef branches via shader_preproc.rs;
// the active define set comes from shader_defines::defines_for_shader().
//
// Mirrors Shader.140.{vert,frag} from $NOESIS_SDK_DIR/Src/Packages/Render/
// GLRenderDevice/Src/. Cross-reference comments call out the GL line whose
// behavior each block ports.
//
// Phase 3 coverage so far: PATH_SOLID, PATH_AA_SOLID, MASK, RGBA, CLEAR.
// Pattern/Linear/Radial/SDF/Opacity/Shadow/Blur/Downsample/Upsample need
// samplers + the resource map from Phase 4 → 6 and land in those phases.

// ─── Uniforms ──────────────────────────────────────────────────────────────
// cbuffer0_vs[16] (projection, 64B) + cbuffer1_vs[2] (glyph atlas size, 8B
// padded to vec4) packed into one struct so a single dynamic-offset bind group
// covers both. The SDF vertex shader needs cbuffer1_vs.xy to scale uv1 into
// glyph-atlas texel coords (`st1` below); other shaders ignore it.
struct VsUniforms {
    projection: mat4x4<f32>,
    glyph_size: vec4<f32>,
}

@group(0) @binding(0) var<uniform> vs_uniforms: VsUniforms;

// cbuffer0_ps[8] in Shader.140.frag — first eight pixel-shader floats. Only
// EFFECT_RGBA reads from it directly so far (uses values[0] as an RGBA fill).
// Always declared so every pipeline shares one bind-group layout.
struct PsUniforms0 {
    values: array<vec4<f32>, 2>,
}

@group(1) @binding(0) var<uniform> ps_uniforms0: PsUniforms0;

// cbuffer1_ps in Shader.140.frag — a second pixel-shader constant block, only
// read by EFFECT_SHADOW / EFFECT_BLUR. Noesis declares it as float[128] but the
// shadow/blur effects touch at most the first 7 floats, so we bind two vec4s
// (values[0] = cb[0..3], values[1] = cb[4..7]). Packed into group(1) binding(1)
// rather than a new bind group so the layout stays within the 4-group
// downlevel-defaults limit. Gated by HAS_CBUFFER1_PS; the shared pipeline
// layout always declares the binding so non-shadow shaders just leave it unused.
#ifdef HAS_CBUFFER1_PS
struct PsUniforms1 {
    values: array<vec4<f32>, 2>,
}

@group(1) @binding(1) var<uniform> ps_uniforms1: PsUniforms1;
#endif

// Group(2) — a texture+sampler pair consumed by any paint variant that
// reads from a 2D texture (PAINT_PATTERN binds `batch.pattern`, PAINT_LINEAR
// binds `batch.ramps`, etc.). Shaders that don't need a texture still share
// the same pipeline layout; the Rust side binds a dummy bind group at draw
// time since wgpu requires every declared group to be set.
#ifdef HAS_PAINT_TEXTURE
@group(2) @binding(0) var paint_texture: texture_2d<f32>;
@group(2) @binding(1) var paint_sampler: sampler;
#endif

// Group(3) — second texture+sampler pair, used by EFFECT_OPACITY (and the
// pending SHADOW_*, BLUR_*, UPSAMPLE_* effects) for the offscreen-rendered
// "image" of the layer being composited. GL ref: `uniform sampler2D image`.
// Distinct group so existing PATH / SDF pipelines don't have to reason
// about a paint+image bind group; the Rust side binds a dummy at this
// slot for shaders that don't read it.
#ifdef HAS_IMAGE_TEXTURE
@group(3) @binding(0) var image_texture: texture_2d<f32>;
@group(3) @binding(1) var image_sampler: sampler;
#endif

// Group(3) bindings 2/3 — the `shadow` texture+sampler, co-bound with `image`
// for EFFECT_SHADOW / EFFECT_BLUR (GL ref: `uniform sampler2D shadow`). Packed
// into the same group as `image` to stay within the 4-bind-group limit. The
// shared pipeline layout always declares these; OPACITY-class shaders that bind
// only `image` leave them unused (the Rust side binds a dummy at 2/3).
#ifdef HAS_SHADOW_TEXTURE
@group(3) @binding(2) var shadow_texture: texture_2d<f32>;
@group(3) @binding(3) var shadow_sampler: sampler;
#endif

// ─── Vertex I/O ────────────────────────────────────────────────────────────
// shader_location matches the VertexAttr enum index in dm_noesis. Each
// attribute is independently gated by its HAS_* define so we can produce all
// 16 vertex-format combinations from one source.

struct VsIn {
    @location(0) pos: vec2<f32>,
#ifdef HAS_COLOR
    @location(1) color: vec4<f32>,
#endif
#ifdef HAS_UV0
    @location(2) uv0: vec2<f32>,
#endif
#ifdef HAS_UV1
    @location(3) uv1: vec2<f32>,
#endif
#ifdef HAS_COVERAGE
    @location(4) coverage: f32,
#endif
#ifdef HAS_RECT
    @location(5) rect: vec4<f32>,
#endif
#ifdef HAS_TILE
    @location(6) tile: vec4<f32>,
#endif
#ifdef HAS_IMAGE_POS
    @location(7) image_pos: vec4<f32>,
#endif
}

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
#ifdef HAS_COLOR
    // GL: `flat in vec4 color;` — match with @interpolate(flat).
    @location(0) @interpolate(flat) color: vec4<f32>,
#endif
#ifdef HAS_UV0
    @location(1) uv0: vec2<f32>,
#endif
#ifdef HAS_UV1
    @location(2) uv1: vec2<f32>,
#endif
#ifdef HAS_COVERAGE
    @location(4) coverage: f32,
#endif
#ifdef HAS_RECT
    @location(5) @interpolate(flat) rect: vec4<f32>,
#endif
#ifdef HAS_TILE
    @location(6) @interpolate(flat) tile: vec4<f32>,
#endif
#ifdef HAS_IMAGE_POS
    @location(7) image_pos: vec4<f32>,
#endif
#ifdef HAS_ST1
    // SDF only. uv1 in glyph-atlas texel space (uv1 × glyph_size). The
    // fragment uses dFdx(st1) to size the AA window per fragment.
    @location(3) st1: vec2<f32>,
#endif
#ifdef DOWNSAMPLE
    // EFFECT_DOWNSAMPLE box-filters four taps; the vertex shader spreads
    // attr_uv0 ± attr_uv1 into uv0..uv3 (GL Shader.140.vert DOWNSAMPLE block).
    @location(8) uv2: vec2<f32>,
    @location(9) uv3: vec2<f32>,
#endif
}

// ─── Vertex shader ─────────────────────────────────────────────────────────
@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    // Noesis stores Matrix4 row-major (GetData()[i*4 + j] = row i, col j) and
    // uploads it verbatim to cbuffer0_vs. WGSL's mat4x4<f32> loads the 16
    // floats as column-major, which transposes the stored matrix relative
    // to the logical one. Right-multiplying `v * M` recovers the logical
    // transform (matches the GL 140 reference `vec4(pos, 0, 1) * mat4(...)`).
    out.clip_position = vec4<f32>(in.pos, 0.0, 1.0) * vs_uniforms.projection;
#ifdef HAS_COLOR
    out.color = in.color;
#endif
#ifdef DOWNSAMPLE
    // 4-tap box filter offsets (GL Shader.140.vert DOWNSAMPLE): uv0 is the
    // tap centre, uv1 carries the ±offset. Overrides the plain HAS_UV0/UV1
    // pass-through below (guarded by #ifndef DOWNSAMPLE).
    out.uv0 = in.uv0 + vec2<f32>(in.uv1.x, in.uv1.y);
    out.uv1 = in.uv0 + vec2<f32>(in.uv1.x, -in.uv1.y);
    out.uv2 = in.uv0 + vec2<f32>(-in.uv1.x, in.uv1.y);
    out.uv3 = in.uv0 + vec2<f32>(-in.uv1.x, -in.uv1.y);
#endif
#ifndef DOWNSAMPLE
#ifdef HAS_UV0
    out.uv0 = in.uv0;
#endif
#ifdef HAS_UV1
    out.uv1 = in.uv1;
#endif
#endif
#ifdef HAS_COVERAGE
    out.coverage = in.coverage;
#endif
#ifdef HAS_RECT
    out.rect = in.rect;
#endif
#ifdef HAS_TILE
    out.tile = in.tile;
#endif
#ifdef HAS_IMAGE_POS
    out.image_pos = in.image_pos;
#endif
#ifdef HAS_ST1
    // Mirrors GL ref `st1 = vec2(attr_uv1 * vec2(cbuffer1_vs[0], cbuffer1_vs[1]))`.
    out.st1 = in.uv1 * vs_uniforms.glyph_size.xy;
#endif
    return out;
}

// ─── Fragment shader ───────────────────────────────────────────────────────
//
// Effect-only paths (RGBA, MASK, CLEAR) return immediately. Effects that
// consume a paint (PATH, PATH_AA) compute the paint first and then apply.
// The trailing `return vec4<f32>(0.0)` is a guaranteed-unreachable fallback
// that keeps WGSL happy when the active branch is the only one with a return.
//
// EFFECT_SDF_LCD needs a dual-source fragment output, so it gets its own
// `fs_main` below; the single-target body here is gated out for that variant.
#ifndef EFFECT_SDF_LCD
@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
#ifdef EFFECT_RGBA
    return ps_uniforms0.values[0];
#endif

#ifdef EFFECT_MASK
    return vec4<f32>(1.0);
#endif

#ifdef EFFECT_CLEAR
    return vec4<f32>(0.0);
#endif

#ifdef PAINT_SOLID
    let paint = in.color;
    let opacity = 1.0;
#endif

// ─── PAINT_PATTERN × { plain | CLAMP | REPEAT | MIRROR_U | MIRROR_V | MIRROR } ──
// Each variant below declares its own `paint` + `opacity`. The shader-define
// set picks exactly one sub-variant, so after preprocessing only one block
// survives and the remaining fn body references a single `paint` / `opacity`.
//
// The wrap variants (everything except PAINT_PATTERN_PLAIN) expect a `rect`
// vertex attribute (normalized bounding rect the paint clamps to — drawn as
// `inside * textureSample(...)`, so out-of-rect fragments contribute zero).
// REPEAT / MIRROR* additionally consume a `tile` attribute giving the
// per-primitive tile origin + size for the UV wrap math. GL reference:
// Shader.140.frag — PAINT_PATTERN block, CLAMP_PATTERN / REPEAT_PATTERN /
// MIRRORU_PATTERN / MIRRORV_PATTERN / MIRROR_PATTERN subblocks.

#ifdef PAINT_PATTERN_PLAIN
    // Plain PAINT_PATTERN — the SamplerState handles wrap/filter.
    let paint = textureSample(paint_texture, paint_sampler, in.uv0);
    let opacity = ps_uniforms0.values[0].x;
#endif

#ifdef CLAMP_PATTERN
    // Explicit clamp to `rect`. Fragments outside the rect discard (zero
    // contribution) via the inside mask — cheaper than relying on the
    // sampler's wrap mode, and lets Noesis place multiple atlased patterns
    // in a single texture.
    let clamped_uv = clamp(in.uv0, in.rect.xy, in.rect.zw);
    let inside = select(0.0, 1.0, all(in.uv0 == clamped_uv));
    let paint = inside * textureSample(paint_texture, paint_sampler, in.uv0);
    let opacity = ps_uniforms0.values[0].x;
#endif

#ifdef REPEAT_PATTERN
    // `tile` = (origin.xy, size.zw). Normalise uv into tile-local space,
    // `fract` to wrap, then lift back into pattern UV space and clamp by
    // `rect`. `textureSampleGrad` preserves the original screen-space
    // derivatives so the sampler picks the right mip despite the UV wrap.
    let raw = (in.uv0 - in.tile.xy) / in.tile.zw;
    let wrap = fract(raw);
    let uv = wrap * in.tile.zw + in.tile.xy;
    let clamped_uv = clamp(uv, in.rect.xy, in.rect.zw);
    let inside = select(0.0, 1.0, all(uv == clamped_uv));
    let paint = inside
        * textureSampleGrad(paint_texture, paint_sampler, uv, dpdx(in.uv0), dpdy(in.uv0));
    let opacity = ps_uniforms0.values[0].x;
#endif

#ifdef MIRRORU_PATTERN
    // Triangle-wave on U, plain fract on V. `abs(x - 2*floor((x-1)/2) - 2)`
    // is the SDK's branchless triangle-wave of period 2 in [0,2]; clamped
    // back into tile space below.
    let raw = (in.uv0 - in.tile.xy) / in.tile.zw;
    let wrap = vec2<f32>(
        abs(raw.x - 2.0 * floor((raw.x - 1.0) * 0.5) - 2.0),
        fract(raw.y),
    );
    let uv = wrap * in.tile.zw + in.tile.xy;
    let clamped_uv = clamp(uv, in.rect.xy, in.rect.zw);
    let inside = select(0.0, 1.0, all(uv == clamped_uv));
    let paint = inside
        * textureSampleGrad(paint_texture, paint_sampler, uv, dpdx(in.uv0), dpdy(in.uv0));
    let opacity = ps_uniforms0.values[0].x;
#endif

#ifdef MIRRORV_PATTERN
    // Mirror V only.
    let raw = (in.uv0 - in.tile.xy) / in.tile.zw;
    let wrap = vec2<f32>(
        fract(raw.x),
        abs(raw.y - 2.0 * floor((raw.y - 1.0) * 0.5) - 2.0),
    );
    let uv = wrap * in.tile.zw + in.tile.xy;
    let clamped_uv = clamp(uv, in.rect.xy, in.rect.zw);
    let inside = select(0.0, 1.0, all(uv == clamped_uv));
    let paint = inside
        * textureSampleGrad(paint_texture, paint_sampler, uv, dpdx(in.uv0), dpdy(in.uv0));
    let opacity = ps_uniforms0.values[0].x;
#endif

#ifdef MIRROR_PATTERN
    // Mirror both axes.
    let raw = (in.uv0 - in.tile.xy) / in.tile.zw;
    let wrap = abs(raw - 2.0 * floor((raw - vec2<f32>(1.0)) * 0.5) - vec2<f32>(2.0));
    let uv = wrap * in.tile.zw + in.tile.xy;
    let clamped_uv = clamp(uv, in.rect.xy, in.rect.zw);
    let inside = select(0.0, 1.0, all(uv == clamped_uv));
    let paint = inside
        * textureSampleGrad(paint_texture, paint_sampler, uv, dpdx(in.uv0), dpdy(in.uv0));
    let opacity = ps_uniforms0.values[0].x;
#endif

#ifdef PAINT_LINEAR
    // Noesis bakes per-gradient ramps into a 2D texture atlas, one row
    // per gradient. `uv0.x` is the gradient parameter (0..1 along the
    // gradient axis); `uv0.y` picks the atlas row. Matches the GL 140
    // reference: `vec4 paint = texture(ramps, uv0); opacity_ = cbuffer0_ps[0];`
    let paint = textureSample(paint_texture, paint_sampler, in.uv0);
    let opacity = ps_uniforms0.values[0].x;
#endif

#ifdef PAINT_RADIAL
    // Mirrors Shader.140.frag PAINT_RADIAL block. cbuffer0_ps is packed
    // into ps_uniforms0.values as (values[0] = cb[0..3], values[1] = cb[4..7]),
    // so:
    //   cb[0..2] — coefficients for the radial parameter `u`
    //   cb[3]    — opacity
    //   cb[4..5] — `dd` term coefficients (focal-offset radial)
    //   cb[6]    — ramp atlas row (v coordinate into `ramps`)
    // Noesis supplies uv0 in a focal-relative gradient space; the shader
    // maps that to a radius and reads the ramp the rest.
    let cb0 = ps_uniforms0.values[0];
    let cb1 = ps_uniforms0.values[1];
    let dd = cb1.x * in.uv0.x - cb1.y * in.uv0.y;
    let r = sqrt(in.uv0.x * in.uv0.x + in.uv0.y * in.uv0.y - dd * dd);
    let u = cb0.x * in.uv0.x + cb0.y * in.uv0.y + cb0.z * r;
    let paint = textureSample(paint_texture, paint_sampler, vec2<f32>(u, cb1.z));
    let opacity = cb0.w;
#endif

#ifdef EFFECT_PATH
    return opacity * paint;
#endif

#ifdef EFFECT_PATH_AA
    return (opacity * in.coverage) * paint;
#endif

#ifdef EFFECT_OPACITY
    // GL ref Shader.140.frag EFFECT_OPACITY:
    //   fragColor = texture(image, uv1) * (opacity_ * paint.a)
    // The "image" texture is an offscreen-rendered pass of the layer
    // being composited (Noesis writes it before this draw); `paint.a`
    // is the per-pixel opacity multiplier (typically the layer's mask
    // alpha or a solid color whose alpha is the opacity). The PATH /
    // PATH_AA paths use `opacity * paint`; this one swaps `paint.rgb`
    // for the offscreen sample, so colour comes from the layer and
    // opacity from `(opacity * paint.a)`.
    return textureSample(image_texture, image_sampler, in.uv1)
        * (opacity * paint.a);
#endif

#ifdef EFFECT_SHADOW
    // GL ref Shader.140.frag EFFECT_SHADOW:
    //   shadowColor = cbuffer1_ps[0..3]
    //   offset      = (cbuffer1_ps[4], -cbuffer1_ps[5])
    //   uv          = clamp(uv1 - offset, rect.xy, rect.zw)
    //   alpha       = mix(image(uv).a, shadow(uv).a, cbuffer1_ps[6])
    //   img         = image(clamp(uv1, rect.xy, rect.zw))
    //   fragColor   = (img + (1 - img.a) * (shadowColor * alpha)) * (opacity * paint.a)
    // `image` is the layer's offscreen render; `shadow` is the blurred-alpha
    // pass. The drop shadow is composited *under* the layer (premultiplied
    // `1 - img.a` weight), then scaled by the effect's overall opacity (the
    // paint alpha carries the per-pixel opacity, opacity_ the global scalar).
    let shadow_color = ps_uniforms1.values[0];
    let shadow_offset = vec2<f32>(ps_uniforms1.values[1].x, -ps_uniforms1.values[1].y);
    let shadow_uv = clamp(in.uv1 - shadow_offset, in.rect.xy, in.rect.zw);
    let shadow_alpha = mix(
        textureSample(image_texture, image_sampler, shadow_uv).a,
        textureSample(shadow_texture, shadow_sampler, shadow_uv).a,
        ps_uniforms1.values[1].z,
    );
    let shadow_img = textureSample(
        image_texture, image_sampler, clamp(in.uv1, in.rect.xy, in.rect.zw));
    return (shadow_img + (1.0 - shadow_img.a) * (shadow_color * shadow_alpha))
        * (opacity * paint.a);
#endif

#ifdef EFFECT_BLUR
    // GL ref Shader.140.frag EFFECT_BLUR:
    //   fragColor = mix(image(uv1), shadow(uv1), cbuffer1_ps[0]) * (opacity * paint.a)
    // `image` is the unblurred layer, `shadow` the blurred pass; cbuffer1_ps[0]
    // crossfades between them (full blur = 1.0). Drives the blur resolve.
    return mix(
        textureSample(image_texture, image_sampler, in.uv1),
        textureSample(shadow_texture, shadow_sampler, in.uv1),
        ps_uniforms1.values[0].x,
    ) * (opacity * paint.a);
#endif

#ifdef EFFECT_DOWNSAMPLE
    // GL ref Shader.140.frag EFFECT_DOWNSAMPLE: average four taps of the
    // source (`pattern`, group 2) at the offset UVs computed in the VS. Halves
    // resolution per pass; the blur/effect resolve chain ping-pongs this.
    return (textureSample(paint_texture, paint_sampler, in.uv0)
        + textureSample(paint_texture, paint_sampler, in.uv1)
        + textureSample(paint_texture, paint_sampler, in.uv2)
        + textureSample(paint_texture, paint_sampler, in.uv3))
        * 0.25;
#endif

#ifdef EFFECT_UPSAMPLE
    // GL ref Shader.140.frag EFFECT_UPSAMPLE:
    //   mix(texture(image, uv1), texture(pattern, uv0), color.a)
    // `image` (group 3) is the lower-resolution accumulated pass; `pattern`
    // (group 2) is the matching-resolution source; color.a is the blend weight.
    return mix(
        textureSample(image_texture, image_sampler, in.uv1),
        textureSample(paint_texture, paint_sampler, in.uv0),
        in.color.a,
    );
#endif

#ifdef EFFECT_SDF
    // Mirrors GL ref Shader.140.frag EFFECT_SDF block:
    //   distance = SDF_SCALE * (texture(glyphs, uv1).r - SDF_BIAS)
    //   gradLen  = length(dFdx(st1))
    //   scale    = 1 / gradLen
    //   base     = SDF_BASE_DEV * (1 - (clamp(scale, MIN, MAX) - MIN) / (MAX - MIN))
    //   range    = SDF_AA_FACTOR * gradLen
    //   alpha    = smoothstep(base - range, base + range, distance)
    //   fragColor = (alpha * opacity) * paint
    let SDF_SCALE: f32 = 7.96875;
    let SDF_BIAS: f32 = 0.50196078431;
    let SDF_AA_FACTOR: f32 = 0.65;
    let SDF_BASE_MIN: f32 = 0.125;
    let SDF_BASE_MAX: f32 = 0.25;
    let SDF_BASE_DEV: f32 = -0.65;

    let distance = SDF_SCALE * (textureSample(paint_texture, paint_sampler, in.uv1).r - SDF_BIAS);
    let gradLen = length(dpdx(in.st1));
    let scale = 1.0 / gradLen;
    let base = SDF_BASE_DEV
        * (1.0 - (clamp(scale, SDF_BASE_MIN, SDF_BASE_MAX) - SDF_BASE_MIN)
                  / (SDF_BASE_MAX - SDF_BASE_MIN));
    let range = SDF_AA_FACTOR * gradLen;
    let alpha = smoothstep(base - range, base + range, distance);
    return (alpha * opacity) * paint;
#endif

    return vec4<f32>(0.0);
}
#endif

// ─── EFFECT_SDF_LCD — subpixel (LCD) text via dual-source blending ──────────
//
// Single-channel SDF text with per-subpixel-channel coverage. The SDK ships no
// GL/VK reference for this path (those devices set `subpixelRendering = false`),
// so this follows the standard single-channel-SDF LCD technique: sample the
// distance field at three horizontally-staggered positions one-third of a
// screen pixel apart, turn each into a coverage value with the same SDF
// smoothstep `SDF_SOLID` uses, and emit those as the R/G/B subpixel coverages.
//
// Output is dual-source:
//   blend_src(0) = paint.rgb premultiplied by per-channel coverage (+ paint.a)
//   blend_src(1) = the per-channel coverage itself
// composited with the `SrcOver_Dual` blend (`cs + cd*(1 - src1)` per channel),
// giving `paint*cov + dst*(1 - cov)` independently for R/G/B — the subpixel
// blend an LCD panel's stripe layout expects.
#ifdef EFFECT_SDF_LCD
struct FsLcdOut {
    @location(0) @blend_src(0) color: vec4<f32>,
    @location(0) @blend_src(1) coverage: vec4<f32>,
}

@fragment
fn fs_main(in: VsOut) -> FsLcdOut {
    let paint = in.color;

    let SDF_SCALE: f32 = 7.96875;
    let SDF_BIAS: f32 = 0.50196078431;
    let SDF_AA_FACTOR: f32 = 0.65;
    let SDF_BASE_MIN: f32 = 0.125;
    let SDF_BASE_MAX: f32 = 0.25;
    let SDF_BASE_DEV: f32 = -0.65;

    // AA window sizing — identical to EFFECT_SDF, from the st1 gradient.
    let gradLen = length(dpdx(in.st1));
    let scale = 1.0 / gradLen;
    let base = SDF_BASE_DEV
        * (1.0 - (clamp(scale, SDF_BASE_MIN, SDF_BASE_MAX) - SDF_BASE_MIN)
                  / (SDF_BASE_MAX - SDF_BASE_MIN));
    let range = SDF_AA_FACTOR * gradLen;

    // One-third-of-a-pixel horizontal stagger in glyph-atlas UV space. dpdx(uv1)
    // is the UV change per screen pixel in x; the R/G/B stripes sit at -1/3, 0,
    // +1/3 pixel.
    let duv = dpdx(in.uv1) * (1.0 / 3.0);
    let dr = SDF_SCALE * (textureSample(paint_texture, paint_sampler, in.uv1 - duv).r - SDF_BIAS);
    let dg = SDF_SCALE * (textureSample(paint_texture, paint_sampler, in.uv1).r - SDF_BIAS);
    let db = SDF_SCALE * (textureSample(paint_texture, paint_sampler, in.uv1 + duv).r - SDF_BIAS);
    let cov = vec3<f32>(
        smoothstep(base - range, base + range, dr),
        smoothstep(base - range, base + range, dg),
        smoothstep(base - range, base + range, db),
    );
    let cov_a = (cov.r + cov.g + cov.b) * (1.0 / 3.0);

    var out: FsLcdOut;
    // Premultiplied so the dual blend's `One` src factor leaves it untouched.
    out.color = vec4<f32>(paint.rgb * cov, paint.a * cov_a);
    out.coverage = vec4<f32>(cov * paint.a, cov_a * paint.a);
    return out;
}
#endif
