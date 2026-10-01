// `@blend_src` needs this enable directive, which must precede every other
// declaration. Gated so only the LCD variant requires DUAL_SOURCE_BLENDING.
#ifdef EFFECT_SDF_LCD
enable dual_source_blending;
#endif

// One source for every Noesis shader. shader_preproc.rs strips the #ifdef
// branches using the define set from shader_defines::defines_for_shader().
//
// Ported from Shader.140.{vert,frag} in $NOESIS_SDK_DIR/Src/Packages/Render/
// GLRenderDevice/Src/; "GL ref" comments quote the GL code a block ports.

// ─── Uniforms ───
// cbuffer0_vs[16] (projection, 64B) + cbuffer1_vs[2] (glyph atlas size, 8B
// padded to vec4) packed into one struct so a single dynamic-offset bind group
// covers both. The SDF vertex shader needs cbuffer1_vs.xy to scale uv1 into
// glyph-atlas texel coords (`st1` below); other shaders ignore it.
struct VsUniforms {
    projection: mat4x4<f32>,
    glyph_size: vec4<f32>,
}

@group(0) @binding(0) var<uniform> vs_uniforms: VsUniforms;

// cbuffer0_ps[8]: values[0] is the fill color for EFFECT_RGBA, and
// values[0].x the opacity for the pattern and linear paints. PAINT_RADIAL
// documents its own layout.
struct PsUniforms0 {
    values: array<vec4<f32>, 2>,
}

@group(1) @binding(0) var<uniform> ps_uniforms0: PsUniforms0;

// cbuffer1_ps: read only by EFFECT_SHADOW / EFFECT_BLUR. Noesis declares
// float[128], but those effects use at most the first 7 floats, so two vec4s
// (values[0] = cb[0..3], values[1] = cb[4..7]). Shares group(1) to stay within
// the downlevel 4-bind-group limit.
#ifdef HAS_CBUFFER1_PS
struct PsUniforms1 {
    values: array<vec4<f32>, 2>,
}

@group(1) @binding(1) var<uniform> ps_uniforms1: PsUniforms1;
#endif

// group(2): the paint texture. Holds `pattern`, `ramps`, or `glyphs`
// depending on the shader, and the DOWNSAMPLE/UPSAMPLE source. Shaders that
// don't read it get a dummy bind group.
#ifdef HAS_PAINT_TEXTURE
@group(2) @binding(0) var paint_texture: texture_2d<f32>;
@group(2) @binding(1) var paint_sampler: sampler;
#endif

// group(3) bindings 0/1: `image`, the offscreen render of the layer being
// composited. Read by EFFECT_OPACITY, UPSAMPLE, SHADOW, and BLUR.
#ifdef HAS_IMAGE_TEXTURE
@group(3) @binding(0) var image_texture: texture_2d<f32>;
@group(3) @binding(1) var image_sampler: sampler;
#endif

// group(3) bindings 2/3: the blurred `shadow` pass, read by EFFECT_SHADOW and
// EFFECT_BLUR. Shares `image`'s group to stay within the 4-bind-group limit.
#ifdef HAS_SHADOW_TEXTURE
@group(3) @binding(2) var shadow_texture: texture_2d<f32>;
@group(3) @binding(3) var shadow_sampler: sampler;
#endif

// ─── Vertex I/O ───
// VsIn locations are the noesis_runtime VertexAttr indices (see
// vertex_layout.rs); each attribute is gated by its own HAS_* define.

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
    // GL ref: `flat in vec4 color;`
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

// ─── Vertex shader ───
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
    // GL ref: `st1 = vec2(attr_uv1 * vec2(cbuffer1_vs[0], cbuffer1_vs[1]))`
    out.st1 = in.uv1 * vs_uniforms.glyph_size.xy;
#endif
    return out;
}

// ─── Fragment shader ───
//
// Each variant keeps one PAINT_* block (defining `paint` and `opacity`) and
// one EFFECT_* block that returns. The trailing `return vec4<f32>(0.0)` is
// unreachable; it keeps the function well-formed for every define set.
// EFFECT_SDF_LCD has its own dual-output `fs_main` below.
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

// ─── Pattern paints ───
// The wrap variants (all but PAINT_PATTERN_PLAIN) read a `rect` attribute, the
// UV bounds of the pattern; fragments outside it contribute zero. REPEAT and
// MIRROR* also read `tile`, the tile origin (xy) and size (zw) for the wrap.
// GL ref: Shader.140.frag PAINT_PATTERN and its *_PATTERN subblocks.

#ifdef PAINT_PATTERN_PLAIN
    // The sampler handles wrap and filtering.
    let paint = textureSample(paint_texture, paint_sampler, in.uv0);
    let opacity = ps_uniforms0.values[0].x;
#endif

#ifdef CLAMP_PATTERN
    // Masking to `rect` in-shader lets Noesis atlas several patterns in one
    // texture, which a sampler wrap mode can't do.
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
    // GL ref Shader.140.frag PAINT_RADIAL. cbuffer0_ps layout
    // (values[0] = cb[0..3], values[1] = cb[4..7]):
    //   cb[0..2]: coefficients for the gradient parameter `u`
    //   cb[3]:    opacity
    //   cb[4..5]: focal-offset `dd` coefficients
    //   cb[6]:    ramp atlas row
    // uv0 is in focal-relative gradient space.
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
    // Color comes from the layer; the paint only contributes its alpha
    // (an opacity mask or a solid opacity).
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
    // The shadow composites under the layer (premultiplied `1 - img.a`).
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
    // cbuffer1_ps[0] crossfades from the unblurred layer (0) to the blur (1).
    return mix(
        textureSample(image_texture, image_sampler, in.uv1),
        textureSample(shadow_texture, shadow_sampler, in.uv1),
        ps_uniforms1.values[0].x,
    ) * (opacity * paint.a);
#endif

#ifdef EFFECT_DOWNSAMPLE
    // GL ref Shader.140.frag EFFECT_DOWNSAMPLE: box filter of four taps of
    // the group(2) source at the VS-computed UVs.
    return (textureSample(paint_texture, paint_sampler, in.uv0)
        + textureSample(paint_texture, paint_sampler, in.uv1)
        + textureSample(paint_texture, paint_sampler, in.uv2)
        + textureSample(paint_texture, paint_sampler, in.uv3))
        * 0.25;
#endif

#ifdef EFFECT_UPSAMPLE
    // GL ref Shader.140.frag EFFECT_UPSAMPLE:
    //   mix(texture(image, uv1), texture(pattern, uv0), color.a)
    // `image` (group 3) is the lower-resolution pass, group(2) the
    // same-resolution source.
    return mix(
        textureSample(image_texture, image_sampler, in.uv1),
        textureSample(paint_texture, paint_sampler, in.uv0),
        in.color.a,
    );
#endif

#ifdef EFFECT_SDF
    // GL ref Shader.140.frag EFFECT_SDF:
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

// ─── EFFECT_SDF_LCD: subpixel text via dual-source blending ───
//
// The SDK has no GL/VK reference for this path (those devices report no
// subpixel rendering). This is the usual single-channel SDF LCD technique:
// sample the distance field at three points a third of a screen pixel apart
// horizontally, and use each one's SDF_SOLID-style coverage for one of R/G/B.
//
// blend_src(0) is paint premultiplied by per-channel coverage; blend_src(1) is
// the coverage. The SrcOver_Dual blend (`cs + cd*(1 - src1)`) then gives
// `paint*cov + dst*(1 - cov)` per channel.
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

    // AA window sizing, as in EFFECT_SDF.
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
