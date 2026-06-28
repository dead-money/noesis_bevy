//! Build a `wgpu::VertexBufferLayout` from Noesis's `VertexFormat` enum +
//! lookup tables (`ATTRIBUTES_FOR_FORMAT`, `TYPE_FOR_ATTR`, `SIZE_FOR_TYPE`,
//! `SIZE_FOR_FORMAT` from `noesis_runtime::render_device::types`).
//!
//! Each Noesis vertex format is a bitmask of [`VertexAttr`] values; the
//! attributes appear in the buffer in `VertexAttr` enum order with no padding.
//! `shader_location` matches the `VertexAttr` index — that's the convention
//! `shaders/noesis.wgsl` uses.
//!
//! [`VertexAttr`]: noesis_runtime::render_device::types::VertexAttr

use noesis_runtime::render_device::types::{
    ATTRIBUTES_FOR_FORMAT, SIZE_FOR_FORMAT, SIZE_FOR_TYPE, TYPE_FOR_ATTR, VERTEX_ATTR_COUNT,
};

/// Owned attribute list for the `format_idx` vertex format — ordering and
/// `shader_location` match the `noesis.wgsl` `VsIn` struct.
#[must_use]
pub fn attributes_for_format(format_idx: u8) -> Vec<wgpu::VertexAttribute> {
    let mask = ATTRIBUTES_FOR_FORMAT[format_idx as usize];
    let mut attrs = Vec::new();
    let mut offset: u64 = 0;
    for attr_bit in 0..VERTEX_ATTR_COUNT as u8 {
        if (mask & (1 << attr_bit)) == 0 {
            continue;
        }
        let type_idx = TYPE_FOR_ATTR[attr_bit as usize];
        attrs.push(wgpu::VertexAttribute {
            offset,
            shader_location: u32::from(attr_bit),
            format: wgpu_format_for_type(type_idx),
        });
        offset += u64::from(SIZE_FOR_TYPE[type_idx as usize]);
    }
    debug_assert_eq!(
        offset,
        u64::from(SIZE_FOR_FORMAT[format_idx as usize]),
        "vertex layout stride mismatch for format {format_idx}"
    );
    attrs
}

#[must_use]
pub fn stride_for_format(format_idx: u8) -> u64 {
    u64::from(SIZE_FOR_FORMAT[format_idx as usize])
}

fn wgpu_format_for_type(type_idx: u8) -> wgpu::VertexFormat {
    match type_idx {
        0 => wgpu::VertexFormat::Float32,   // VertexAttrType::Float
        1 => wgpu::VertexFormat::Float32x2, // VertexAttrType::Float2
        2 => wgpu::VertexFormat::Float32x4, // VertexAttrType::Float4
        3 => wgpu::VertexFormat::Unorm8x4,  // VertexAttrType::UByte4Norm
        4 => wgpu::VertexFormat::Unorm16x4, // VertexAttrType::UShort4Norm
        other => panic!("unknown VertexAttrType: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pos_color_layout() {
        // VertexFormat::PosColor (idx 1): Pos@0 (Float32x2, 8B) + Color@1 (Unorm8x4, 4B)
        let attrs = attributes_for_format(1);
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].shader_location, 0);
        assert_eq!(attrs[0].offset, 0);
        assert_eq!(attrs[0].format, wgpu::VertexFormat::Float32x2);
        assert_eq!(attrs[1].shader_location, 1);
        assert_eq!(attrs[1].offset, 8);
        assert_eq!(attrs[1].format, wgpu::VertexFormat::Unorm8x4);
        assert_eq!(stride_for_format(1), 12);
    }

    #[test]
    fn pos_tex0_rect_tile_layout() {
        // VertexFormat::PosTex0RectTile (idx 4):
        //   Pos@0 (Float32x2, 8) + Tex0@2 (Float32x2, 8) +
        //   Rect@5 (Unorm16x4, 8) + Tile@6 (Float32x4, 16) = 40
        let attrs = attributes_for_format(4);
        assert_eq!(attrs.len(), 4);
        assert_eq!(attrs[0].shader_location, 0);
        assert_eq!(attrs[1].shader_location, 2);
        assert_eq!(attrs[2].shader_location, 5);
        assert_eq!(attrs[3].shader_location, 6);
        assert_eq!(stride_for_format(4), 40);
    }
}
