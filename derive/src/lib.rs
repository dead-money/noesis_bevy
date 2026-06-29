//! `#[derive(NoesisViewModel)]` generates the glue that binds a plain Rust
//! struct to XAML `{Binding field_name}` by field name, with two-way
//! writeback, through `noesis_bevy`'s plain-VM bridge.
//!
//! The derive maps each field to a Noesis-reflected property:
//!
//! | Rust field type        | Noesis property type |
//! |------------------------|----------------------|
//! | `f32`, `f64`           | `Double`             |
//! | `i32`, `u32`           | `Int32`              |
//! | `bool`                 | `Bool`               |
//! | `String`               | `String`             |
//!
//! Unsupported field types are a compile error; annotate them `#[noesis(skip)]`
//! to exclude them from the view model. The Noesis type name defaults to the
//! struct's identifier; override with `#[noesis(name = "...")]`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr, Type, parse_macro_input, spanned::Spanned};

/// Per-field codegen fragments, derived from the field's Rust type.
struct FieldKind {
    /// `PlainType` variant ident (e.g. `Double`).
    plain_type: proc_macro2::TokenStream,
    /// Build a `PlainValue` from `self.<field>` for the snapshot.
    snapshot: proc_macro2::TokenStream,
    /// Assign `self.<field>` from a matched `PlainValue` in `noesis_apply`.
    apply: proc_macro2::TokenStream,
}

fn field_kind(field: &proc_macro2::TokenStream, ty: &Type) -> Option<FieldKind> {
    let ident = match ty {
        Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string())?,
        _ => return None,
    };
    let pv = quote!(noesis_bevy::plain_vm::PlainValue);
    let pt = quote!(noesis_bevy::plain_vm::PlainType);
    Some(match ident.as_str() {
        "f32" => FieldKind {
            plain_type: quote!(#pt::Double),
            snapshot: quote!(#pv::Double(f64::from(self.#field))),
            apply: quote!(if let #pv::Double(v) = value { self.#field = *v as f32; }),
        },
        "f64" => FieldKind {
            plain_type: quote!(#pt::Double),
            snapshot: quote!(#pv::Double(self.#field)),
            apply: quote!(if let #pv::Double(v) = value { self.#field = *v; }),
        },
        "i32" => FieldKind {
            plain_type: quote!(#pt::Int32),
            snapshot: quote!(#pv::Int32(self.#field)),
            apply: quote!(if let #pv::Int32(v) = value { self.#field = *v; }),
        },
        "u32" => FieldKind {
            plain_type: quote!(#pt::Int32),
            snapshot: quote!(#pv::Int32(self.#field as i32)),
            apply: quote!(if let #pv::Int32(v) = value { self.#field = *v as u32; }),
        },
        "bool" => FieldKind {
            plain_type: quote!(#pt::Bool),
            snapshot: quote!(#pv::Bool(self.#field)),
            apply: quote!(if let #pv::Bool(v) = value { self.#field = *v; }),
        },
        "String" => FieldKind {
            plain_type: quote!(#pt::String),
            snapshot: quote!(#pv::String(self.#field.clone())),
            apply: quote!(if let #pv::String(v) = value { self.#field = v.clone(); }),
        },
        _ => return None,
    })
}

/// Whether a field carries `#[noesis(skip)]`.
fn is_skipped(attrs: &[syn::Attribute]) -> bool {
    let mut skip = false;
    for attr in attrs {
        if attr.path().is_ident("noesis") {
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("skip") {
                    skip = true;
                }
                Ok(())
            });
        }
    }
    skip
}

/// Read a struct-level `#[noesis(name = "...")]` override.
fn type_name_override(attrs: &[syn::Attribute]) -> Option<String> {
    let mut name = None;
    for attr in attrs {
        if attr.path().is_ident("noesis") {
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let value = meta.value()?;
                    let lit: LitStr = value.parse()?;
                    name = Some(lit.value());
                }
                Ok(())
            });
        }
    }
    name
}

#[proc_macro_derive(NoesisViewModel, attributes(noesis))]
pub fn derive_noesis_view_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_ident = &input.ident;

    let type_name = type_name_override(&input.attrs).unwrap_or_else(|| struct_ident.to_string());

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return syn::Error::new(
                    input.span(),
                    "NoesisViewModel requires a struct with named fields",
                )
                .to_compile_error()
                .into();
            }
        },
        _ => {
            return syn::Error::new(input.span(), "NoesisViewModel can only derive on a struct")
                .to_compile_error()
                .into();
        }
    };

    let mut prop_entries = Vec::new();
    let mut snapshot_pushes = Vec::new();
    let mut apply_arms = Vec::new();
    let mut index: u32 = 0;

    for field in fields {
        if is_skipped(&field.attrs) {
            continue;
        }
        let ident = field.ident.as_ref().expect("named field");
        let ident_tokens = quote!(#ident);
        let name_str = ident.to_string();
        let Some(kind) = field_kind(&ident_tokens, &field.ty) else {
            return syn::Error::new(
                field.ty.span(),
                "NoesisViewModel: unsupported field type (expected f32, f64, i32, u32, bool, or \
                 String). Annotate the field `#[noesis(skip)]` to exclude it.",
            )
            .to_compile_error()
            .into();
        };
        let FieldKind {
            plain_type,
            snapshot,
            apply,
        } = kind;
        prop_entries.push(quote!((#name_str, #plain_type)));
        snapshot_pushes.push(snapshot);
        apply_arms.push(quote!(#index => { #apply }));
        index += 1;
    }

    let expanded = quote! {
        impl noesis_bevy::plain_vm::NoesisViewModel for #struct_ident {
            fn noesis_type_name() -> &'static str {
                #type_name
            }

            fn noesis_properties() -> &'static [(&'static str, noesis_bevy::plain_vm::PlainType)] {
                &[#(#prop_entries),*]
            }

            fn noesis_snapshot(&self) -> ::std::vec::Vec<noesis_bevy::plain_vm::PlainValue> {
                ::std::vec![#(#snapshot_pushes),*]
            }

            fn noesis_apply(&mut self, prop_index: u32, value: &noesis_bevy::plain_vm::PlainValue) {
                match prop_index {
                    #(#apply_arms),*
                    _ => {}
                }
            }
        }
    };

    expanded.into()
}
