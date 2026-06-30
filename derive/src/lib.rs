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
//! Two struct shapes are supported:
//!
//! * **Named struct**: each field maps to a property named after the *field*
//!   (`title: String` → `{Binding title}`). `#[noesis(skip)]` excludes a field;
//!   `#[noesis(rename = "Title")]` binds a `snake_case` field to a different XAML
//!   property name (e.g. `PascalCase`).
//! * **Newtype tuple struct**: a single-field tuple struct
//!   (`struct Health(f32);`) maps to one property named after the *type*
//!   (`{Binding Health}`). This is the shape the `UiPanel` primitive expects:
//!   spawn `Health(100.0)` on a panel entity and bind `{Binding Health}`.
//!
//! Unsupported field types are a compile error; annotate them `#[noesis(skip)]`
//! to exclude them from the view model. The Noesis type name defaults to the
//! struct's identifier; override with `#[noesis(name = "...")]`. A newtype's
//! property name defaults to the type identifier; override with
//! `#[noesis(as = "Name")]`.

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

/// Read a field-level `#[noesis(rename = "...")]` property-name override, so a
/// snake_case Rust field can bind to a different (e.g. PascalCase) XAML property.
fn field_rename(attrs: &[syn::Attribute]) -> Option<String> {
    let mut name = None;
    for attr in attrs {
        if attr.path().is_ident("noesis") {
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename") {
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

/// Read a `#[noesis(as = "Name")]` property-name override (newtype shape). `as`
/// is a Rust keyword, so it never parses as a `syn` meta path; we scan the
/// attribute's raw tokens for the `as = "<lit>"` triple instead.
fn prop_name_override(attrs: &[syn::Attribute]) -> Option<String> {
    use proc_macro2::TokenTree;
    for attr in attrs {
        if !attr.path().is_ident("noesis") {
            continue;
        }
        let syn::Meta::List(list) = &attr.meta else {
            continue;
        };
        let mut trees = list.tokens.clone().into_iter().peekable();
        while let Some(tree) = trees.next() {
            // keywords are plain idents at the token level, so `as` matches here
            let TokenTree::Ident(ident) = &tree else {
                continue;
            };
            if *ident != "as" {
                continue;
            }
            if let Some(TokenTree::Punct(p)) = trees.peek() {
                if p.as_char() == '=' {
                    trees.next();
                    if let Some(TokenTree::Literal(lit)) = trees.next() {
                        if let Ok(s) = syn::parse_str::<LitStr>(&lit.to_string()) {
                            return Some(s.value());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Append one resolved property's codegen to the running collections.
fn push_field(
    prop_entries: &mut Vec<proc_macro2::TokenStream>,
    snapshot_pushes: &mut Vec<proc_macro2::TokenStream>,
    apply_arms: &mut Vec<proc_macro2::TokenStream>,
    index: &mut u32,
    name_str: &str,
    kind: FieldKind,
) {
    let FieldKind {
        plain_type,
        snapshot,
        apply,
    } = kind;
    let i = *index;
    prop_entries.push(quote!((#name_str, #plain_type)));
    snapshot_pushes.push(snapshot);
    apply_arms.push(quote!(#i => { #apply }));
    *index += 1;
}

fn unsupported_type_error(span: proc_macro2::Span) -> TokenStream {
    syn::Error::new(
        span,
        "NoesisViewModel: unsupported field type (expected f32, f64, i32, u32, bool, or String). \
         Annotate the field `#[noesis(skip)]` to exclude it.",
    )
    .to_compile_error()
    .into()
}

#[proc_macro_derive(NoesisViewModel, attributes(noesis))]
pub fn derive_noesis_view_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_ident = &input.ident;

    let type_name = type_name_override(&input.attrs).unwrap_or_else(|| struct_ident.to_string());

    let fields = match &input.data {
        Data::Struct(s) => &s.fields,
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

    match fields {
        Fields::Named(named) => {
            for field in &named.named {
                if is_skipped(&field.attrs) {
                    continue;
                }
                let ident = field.ident.as_ref().expect("named field");
                let access = quote!(#ident);
                let name_str = field_rename(&field.attrs).unwrap_or_else(|| ident.to_string());
                let Some(kind) = field_kind(&access, &field.ty) else {
                    return unsupported_type_error(field.ty.span());
                };
                push_field(
                    &mut prop_entries,
                    &mut snapshot_pushes,
                    &mut apply_arms,
                    &mut index,
                    &name_str,
                    kind,
                );
            }
        }
        // Newtype: one property named after the *type* (override: `#[noesis(as)]`).
        Fields::Unnamed(unnamed) => {
            if unnamed.unnamed.len() != 1 {
                return syn::Error::new(
                    input.span(),
                    "NoesisViewModel on a tuple struct requires exactly one field \
                     (a newtype, e.g. `struct Health(f32);`)",
                )
                .to_compile_error()
                .into();
            }
            let field = &unnamed.unnamed[0];
            let zero = syn::Index::from(0);
            let access = quote!(#zero);
            let name_str =
                prop_name_override(&input.attrs).unwrap_or_else(|| struct_ident.to_string());
            let Some(kind) = field_kind(&access, &field.ty) else {
                return unsupported_type_error(field.ty.span());
            };
            push_field(
                &mut prop_entries,
                &mut snapshot_pushes,
                &mut apply_arms,
                &mut index,
                &name_str,
                kind,
            );
        }
        Fields::Unit => {
            return syn::Error::new(
                input.span(),
                "NoesisViewModel requires a struct with fields (named or a single-field newtype)",
            )
            .to_compile_error()
            .into();
        }
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
