//! Sets and watches the font properties (size, family, weight, style,
//! stretch) of named text elements in a [`NoesisView`](crate::NoesisView).
//!
//! These are `TextElement` dependency properties. `FontWeight`, `FontStyle`,
//! and `FontStretch` are enums that the generic [`NoesisDp`](crate::dp::NoesisDp)
//! bridge can't read or write, so this bridge offers typed access to all five.
//!
//! Add a [`NoesisTypography`] to the view's camera entity.
//! [`set`](NoesisTypography::set) holds a [`FontStyling`] per `x:Name`;
//! [`watch`](NoesisTypography::watch) lists properties to report through
//! [`NoesisTypographyChanged`].
//!
//! ```ignore
//! use noesis_bevy::{NoesisTypography, FontWeight};
//!
//! commands.entity(view).insert(
//!     NoesisTypography::new()
//!         .font_size("Title", 28.0)
//!         .font_family("Title", "#PT Root UI")
//!         .font_weight("Title", FontWeight::Bold),
//! );
//! ```
//!
//! Whenever the component changes or the view's scene is rebuilt, every
//! [`FontStyling`] in `set` is written again: its `Some` fields are set and its
//! `None` fields are left alone. Removing an entry does not restore the
//! element's previous font. Writes made through `set` are reported by any
//! matching watch, like any other change.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

pub use noesis_runtime::typography::{FontStretch, FontStyle, FontWeight};

/// Font properties to write on one element. `Some` fields are written; `None`
/// fields leave the element's current value alone.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct FontStyling {
    /// `FontSize`, in DIPs.
    pub font_size: Option<f32>,
    /// `FontFamily` source string, as written in XAML: `"Arial"`,
    /// `"Fonts/#PT Root UI"`, or a comma-separated fallback list.
    pub font_family: Option<String>,
    /// `FontWeight`.
    pub font_weight: Option<FontWeight>,
    /// `FontStyle`.
    pub font_style: Option<FontStyle>,
    /// `FontStretch`.
    pub font_stretch: Option<FontStretch>,
}

impl FontStyling {
    pub(crate) fn is_empty(&self) -> bool {
        self.font_size.is_none()
            && self.font_family.is_none()
            && self.font_weight.is_none()
            && self.font_style.is_none()
            && self.font_stretch.is_none()
    }
}

/// A font property to watch. Each reports through the matching
/// [`TypographyValue`] variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypographyField {
    /// `FontSize`.
    FontSize,
    /// `FontFamily`, as its source string.
    FontFamily,
    /// `FontWeight`.
    FontWeight,
    /// `FontStyle`.
    FontStyle,
    /// `FontStretch`.
    FontStretch,
}

/// A font property value read from a live element. The variant names the
/// property.
#[derive(Debug, Clone, PartialEq)]
pub enum TypographyValue {
    /// `FontSize` in DIPs.
    FontSize(f32),
    /// `FontFamily` source string; `None` when the family has no source.
    FontFamily(Option<String>),
    /// `FontWeight`.
    FontWeight(FontWeight),
    /// `FontStyle`.
    FontStyle(FontStyle),
    /// `FontStretch`.
    FontStretch(FontStretch),
}

/// One watched font property on one element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypographyWatch {
    /// `x:Name` of the element (may be scope-qualified, `"Host/Leaf"`).
    pub name: String,
    /// Property to watch.
    pub field: TypographyField,
}

/// A watched font property changed since the previous frame, or its watch was
/// just added. Polled every frame.
#[derive(Message, Debug, Clone)]
pub struct NoesisTypographyChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose property changed.
    pub view: Entity,
    /// `x:Name` of the element whose property changed.
    pub name: String,
    /// Current value; the variant tells which property changed.
    pub value: TypographyValue,
}

/// Font writes and watches for named elements. Add to a
/// [`NoesisView`](crate::NoesisView) camera entity; see the
/// [module docs](self).
///
/// The per-property builders and setters merge into the element's existing
/// [`FontStyling`].
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisTypography {
    /// Font properties per element `x:Name` (may be scope-qualified). Target a
    /// text element (`TextBlock`, `Run`, `TextBox`, a `Control`). A missing name
    /// warns; a property the element doesn't have is skipped and logged at
    /// debug level.
    pub set: HashMap<String, FontStyling>,
    /// Properties to report through [`NoesisTypographyChanged`].
    pub watch: Vec<TypographyWatch>,
}

impl NoesisTypography {
    /// An empty bridge. Chain the builders to fill it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name`'s `FontSize` in DIPs.
    #[must_use]
    pub fn font_size(mut self, name: impl Into<String>, size: f32) -> Self {
        self.entry(name).font_size = Some(size);
        self
    }

    /// Builder: set element `name`'s `FontFamily` from a source string such as
    /// `"Fonts/#PT Root UI"`.
    #[must_use]
    pub fn font_family(mut self, name: impl Into<String>, source: impl Into<String>) -> Self {
        self.entry(name).font_family = Some(source.into());
        self
    }

    /// Builder: set element `name`'s `FontWeight`.
    #[must_use]
    pub fn font_weight(mut self, name: impl Into<String>, weight: FontWeight) -> Self {
        self.entry(name).font_weight = Some(weight);
        self
    }

    /// Builder: set element `name`'s `FontStyle`.
    #[must_use]
    pub fn font_style(mut self, name: impl Into<String>, style: FontStyle) -> Self {
        self.entry(name).font_style = Some(style);
        self
    }

    /// Builder: set element `name`'s `FontStretch`.
    #[must_use]
    pub fn font_stretch(mut self, name: impl Into<String>, stretch: FontStretch) -> Self {
        self.entry(name).font_stretch = Some(stretch);
        self
    }

    /// Builder: replace element `name`'s whole [`FontStyling`].
    #[must_use]
    pub fn styling(mut self, name: impl Into<String>, styling: FontStyling) -> Self {
        self.set.insert(name.into(), styling);
        self
    }

    /// Builder: report changes to element `name`'s `field` through
    /// [`NoesisTypographyChanged`].
    #[must_use]
    pub fn watch(mut self, name: impl Into<String>, field: TypographyField) -> Self {
        self.watch.push(TypographyWatch {
            name: name.into(),
            field,
        });
        self
    }

    /// The `&mut` form of [`font_size`](Self::font_size), for systems that
    /// update the component.
    pub fn set_font_size(&mut self, name: impl Into<String>, size: f32) {
        self.entry(name).font_size = Some(size);
    }

    /// The `&mut` form of [`font_family`](Self::font_family).
    pub fn set_font_family(&mut self, name: impl Into<String>, source: impl Into<String>) {
        self.entry(name).font_family = Some(source.into());
    }

    /// The `&mut` form of [`font_weight`](Self::font_weight).
    pub fn set_font_weight(&mut self, name: impl Into<String>, weight: FontWeight) {
        self.entry(name).font_weight = Some(weight);
    }

    /// The `&mut` form of [`font_style`](Self::font_style).
    pub fn set_font_style(&mut self, name: impl Into<String>, style: FontStyle) {
        self.entry(name).font_style = Some(style);
    }

    /// The `&mut` form of [`font_stretch`](Self::font_stretch).
    pub fn set_font_stretch(&mut self, name: impl Into<String>, stretch: FontStretch) {
        self.entry(name).font_stretch = Some(stretch);
    }

    /// The `&mut` form of [`styling`](Self::styling).
    pub fn set_styling(&mut self, name: impl Into<String>, styling: FontStyling) {
        self.set.insert(name.into(), styling);
    }

    /// The `&mut` form of [`watch`](Self::watch). No-op if `(name, field)` is
    /// already watched.
    pub fn observe(&mut self, name: impl Into<String>, field: TypographyField) {
        let watch = TypographyWatch {
            name: name.into(),
            field,
        };
        if !self.watch.contains(&watch) {
            self.watch.push(watch);
        }
    }

    fn entry(&mut self, name: impl Into<String>) -> &mut FontStyling {
        self.set.entry(name.into()).or_default()
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_typography_bridge(
    views: Query<(Entity, Ref<NoesisTypography>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut changed: MessageWriter<NoesisTypographyChanged>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, typography) in &views {
        if typography.is_changed() || state.scene_rebuilt_this_frame(entity) {
            state.apply_typography_for(entity, &typography.set);
        }
        for (name, value) in state.poll_typography_reads_for(entity, &typography.watch) {
            changed.write(NoesisTypographyChanged {
                view: entity,
                name,
                value,
            });
        }
    }
}

/// Registers the typography bridge. Added by [`crate::NoesisPlugin`].
pub struct NoesisTypographyPlugin;

impl Plugin for NoesisTypographyPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisTypographyChanged>()
            .add_systems(PostUpdate, sync_typography_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_per_name_styling() {
        let t = NoesisTypography::new()
            .font_size("Title", 28.0)
            .font_family("Title", "#PT Root UI")
            .font_weight("Title", FontWeight::Bold)
            .font_style("Sub", FontStyle::Italic)
            .font_stretch("Sub", FontStretch::Condensed);

        let title = t.set.get("Title").expect("Title styling present");
        assert_eq!(title.font_size, Some(28.0));
        assert_eq!(title.font_family.as_deref(), Some("#PT Root UI"));
        assert_eq!(title.font_weight, Some(FontWeight::Bold));
        assert_eq!(title.font_style, None);

        let sub = t.set.get("Sub").expect("Sub styling present");
        assert_eq!(sub.font_style, Some(FontStyle::Italic));
        assert_eq!(sub.font_stretch, Some(FontStretch::Condensed));
        assert_eq!(sub.font_size, None);
    }

    #[test]
    fn last_write_wins_per_field() {
        let t = NoesisTypography::new()
            .font_size("Title", 12.0)
            .font_size("Title", 24.0);
        assert_eq!(t.set.get("Title").unwrap().font_size, Some(24.0));
    }
}
