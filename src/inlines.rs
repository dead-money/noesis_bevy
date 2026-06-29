//! Per-view **formatted text** bridge — give a named `TextBlock` rich inline
//! content (`Run` / `Bold` / `Italic` / `Underline` / `Span` / `Hyperlink` /
//! `LineBreak`) from Bevy, then read the resulting live structure back.
//!
//! This is the [`crate::typography`] bridge's sibling: typography restyles the
//! *font* attached properties of an element, while this bridge replaces a
//! `TextBlock`'s **`Inlines`** — the flow-content tree behind WPF-style
//! `<TextBlock><Run/><Bold>…</Bold></TextBlock>` markup — built entirely in code.
//! (It is distinct from `noesis_runtime::formatted_text`, which is a standalone
//! text *measurement* object, not a `TextBlock`'s content.)
//!
//! Beyond the styled spans, an [`InlineSpec`] can carry a [`TextDecorations`]
//! value (via [`InlineSpec::decorated`], a `Span` with the decoration applied to
//! it and its descendants) and embed an arbitrary `UIElement` in flow content
//! (via [`InlineSpec::ui_container`], an `InlineUIContainer` hosting a child
//! parsed from XAML).
//!
//! Add a [`NoesisInlines`] component to the view's camera entity. Its `set` map is
//! the desired inline tree ([`InlineSpec`]) per `x:Name`, applied whenever the
//! component changes (Bevy change detection). Its `watch` list names `TextBlock`s
//! whose live inline structure to observe; each surfaces as a
//! [`NoesisInlinesChanged`] carrying an [`InlinesReadback`].
//!
//! ```ignore
//! use dm_noesis_bevy::{InlineSpec, NoesisInlines};
//!
//! commands.entity(view).insert(
//!     NoesisInlines::new()
//!         .set("Body", [
//!             InlineSpec::run("Hello "),
//!             InlineSpec::bold([InlineSpec::run("World")]),
//!             InlineSpec::line_break(),
//!             InlineSpec::italic([InlineSpec::run("!")]),
//!         ])
//!         .watching(["Body"]),
//! );
//! ```
//!
//! # Re-apply semantics
//!
//! A changed [`NoesisInlines`] component is fully re-applied: for each named
//! `TextBlock` in `set`, the bridge **clears** the live `InlineCollection`
//! (`InlineCollection::clear`, exposed since runtime 0.10) and repopulates it
//! from the new spec, replacing whatever was there — whether built by an earlier
//! apply or authored in XAML. Editing a spec therefore swaps the rendered
//! content in place without rebuilding the scene.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and applies the writes /
//! polls the reads against that view's live scene — no cross-world queues.

use std::collections::HashMap;
use std::ffi::c_void;

use bevy::prelude::*;
use noesis_runtime::text_inlines::{
    Bold, Hyperlink, Inline, InlineCollection, InlineUIContainer, Italic, LineBreak, Run, Span,
    Underline,
};
use noesis_runtime::view::FrameworkElement;

/// Re-exported from `noesis_runtime`: the `TextDecorations` an inline can carry
/// (see [`InlineSpec::decorated`]).
pub use noesis_runtime::text_inlines::TextDecorations;

use crate::render::{NoesisRenderState, NoesisSet};

// ─────────────────────────────────────────────────────────────────────────────
// Declarative spec
// ─────────────────────────────────────────────────────────────────────────────

/// A declarative description of one inline in a `TextBlock`'s flow content. The
/// `Bold` / `Italic` / `Underline` / `Span` / `Hyperlink` variants nest further
/// inlines; `Run` carries plain text and `LineBreak` forces a break.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineSpec {
    /// A `Run`: a span of plain text.
    Run(String),
    /// A `LineBreak`: forces a line break in flow content.
    LineBreak,
    /// A `Bold` span (renders its children bold).
    Bold(Vec<InlineSpec>),
    /// An `Italic` span (renders its children italic).
    Italic(Vec<InlineSpec>),
    /// An `Underline` span (underlines its children).
    Underline(Vec<InlineSpec>),
    /// A plain `Span` grouping its children with no inherent styling.
    Span(Vec<InlineSpec>),
    /// A `Hyperlink` span with an optional `NavigateUri`.
    Hyperlink {
        /// The `NavigateUri` to set, if any.
        uri: Option<String>,
        /// The hyperlink's child inlines (typically its label `Run`).
        children: Vec<InlineSpec>,
    },
    /// A `Span` carrying a [`TextDecorations`] value applied to it and its
    /// descendants (e.g. `Strikethrough` / `OverLine`). This is how per-inline
    /// `TextDecorations` is expressed in the spec.
    Decorated {
        /// The decoration to apply to the span.
        decoration: TextDecorations,
        /// The decorated span's child inlines.
        children: Vec<InlineSpec>,
    },
    /// An `InlineUIContainer` embedding an arbitrary `UIElement` in flow content.
    /// The child is parsed from `child_xaml` (e.g. `"<Button Content=\"Go\"/>"`,
    /// with the presentation namespace declared) so it is freshly owned by the
    /// container and never collides with an element already in the visual tree.
    UiContainer {
        /// XAML markup parsed into the hosted `UIElement`.
        child_xaml: String,
    },
}

impl InlineSpec {
    /// A plain-text `Run`.
    #[must_use]
    pub fn run(text: impl Into<String>) -> Self {
        Self::Run(text.into())
    }

    /// A `LineBreak`.
    #[must_use]
    pub fn line_break() -> Self {
        Self::LineBreak
    }

    /// A `Bold` span around `children`.
    #[must_use]
    pub fn bold(children: impl IntoIterator<Item = InlineSpec>) -> Self {
        Self::Bold(children.into_iter().collect())
    }

    /// An `Italic` span around `children`.
    #[must_use]
    pub fn italic(children: impl IntoIterator<Item = InlineSpec>) -> Self {
        Self::Italic(children.into_iter().collect())
    }

    /// An `Underline` span around `children`.
    #[must_use]
    pub fn underline(children: impl IntoIterator<Item = InlineSpec>) -> Self {
        Self::Underline(children.into_iter().collect())
    }

    /// A plain `Span` around `children`.
    #[must_use]
    pub fn span(children: impl IntoIterator<Item = InlineSpec>) -> Self {
        Self::Span(children.into_iter().collect())
    }

    /// A `Hyperlink` with `NavigateUri = uri` around `children`.
    #[must_use]
    pub fn hyperlink(
        uri: impl Into<String>,
        children: impl IntoIterator<Item = InlineSpec>,
    ) -> Self {
        Self::Hyperlink {
            uri: Some(uri.into()),
            children: children.into_iter().collect(),
        }
    }

    /// A `Span` with `decoration` applied to it (and its descendants).
    #[must_use]
    pub fn decorated(
        decoration: TextDecorations,
        children: impl IntoIterator<Item = InlineSpec>,
    ) -> Self {
        Self::Decorated {
            decoration,
            children: children.into_iter().collect(),
        }
    }

    /// An `InlineUIContainer` hosting the `UIElement` parsed from `child_xaml`.
    #[must_use]
    pub fn ui_container(child_xaml: impl Into<String>) -> Self {
        Self::UiContainer {
            child_xaml: child_xaml.into(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Live handle tree (built on apply, kept for read-back)
// ─────────────────────────────────────────────────────────────────────────────

/// The live Noesis inline objects the bridge built for one `TextBlock`, mirroring
/// the [`InlineSpec`] tree. Kept in the scene so the read-back can re-read the
/// *live* `Run` text / `Hyperlink` URIs (the runtime exposes no way to wrap a raw
/// `Inline*` from the collection back into a typed handle, so we hold our own).
/// Each handle is also owned (`AddRef`'d) by the collection it was added to, so
/// these stay valid as long as the `TextBlock` keeps them.
pub(crate) enum BuiltInline {
    Run(Run),
    LineBreak(LineBreak),
    Bold(Bold, Vec<BuiltInline>),
    Italic(Italic, Vec<BuiltInline>),
    Underline(Underline, Vec<BuiltInline>),
    Span(Span, Vec<BuiltInline>),
    Hyperlink(Hyperlink, Vec<BuiltInline>),
    /// A decoration-carrying `Span`; the second field is its children. Held
    /// separately from `Span` so the read-back can report its live
    /// [`TextDecorations`].
    Decorated(Span, Vec<BuiltInline>),
    /// An `InlineUIContainer` and the child element it hosts (`None` if the
    /// child XAML failed to parse). The child handle is kept so the read-back
    /// can check the live `Child` against it by pointer identity.
    UiContainer(InlineUIContainer, Option<FrameworkElement>),
}

impl BuiltInline {
    /// Raw `Inline*`, for pointer-identity checks against the live collection.
    fn raw(&self) -> *mut c_void {
        match self {
            BuiltInline::Run(h) => h.raw(),
            BuiltInline::LineBreak(h) => h.raw(),
            BuiltInline::Bold(h, _) => h.raw(),
            BuiltInline::Italic(h, _) => h.raw(),
            BuiltInline::Underline(h, _) => h.raw(),
            BuiltInline::Span(h, _) => h.raw(),
            BuiltInline::Hyperlink(h, _) => h.raw(),
            BuiltInline::Decorated(h, _) => h.raw(),
            BuiltInline::UiContainer(h, _) => h.raw(),
        }
    }

    /// Append this inline to `collection` (which takes its own reference).
    fn add_to(&self, collection: &mut InlineCollection) {
        let _ = match self {
            BuiltInline::Run(h) => collection.add(h),
            BuiltInline::LineBreak(h) => collection.add(h),
            BuiltInline::Bold(h, _) => collection.add(h),
            BuiltInline::Italic(h, _) => collection.add(h),
            BuiltInline::Underline(h, _) => collection.add(h),
            BuiltInline::Span(h, _) => collection.add(h),
            BuiltInline::Hyperlink(h, _) => collection.add(h),
            BuiltInline::Decorated(h, _) => collection.add(h),
            BuiltInline::UiContainer(h, _) => collection.add(h),
        };
    }
}

/// Build the `specs` into freshly-created Noesis inlines, appending each to
/// `collection` (depth-first for nested spans), and return the live handle tree.
pub(crate) fn build_into(
    collection: &mut InlineCollection,
    specs: &[InlineSpec],
) -> Vec<BuiltInline> {
    let mut built = Vec::with_capacity(specs.len());
    for spec in specs {
        let node = build_one(spec);
        node.add_to(collection);
        built.push(node);
    }
    built
}

/// Build a span-like inline: create its handle, populate its nested collection,
/// and wrap both in `$variant`.
macro_rules! build_span_like {
    ($variant:ident, $ctor:expr, $children:expr) => {{
        let handle = $ctor;
        let kids = match handle.inlines() {
            Some(mut col) => build_into(&mut col, $children),
            None => Vec::new(),
        };
        BuiltInline::$variant(handle, kids)
    }};
}

fn build_one(spec: &InlineSpec) -> BuiltInline {
    match spec {
        InlineSpec::Run(text) => BuiltInline::Run(Run::new(text)),
        InlineSpec::LineBreak => BuiltInline::LineBreak(LineBreak::new()),
        InlineSpec::Bold(children) => build_span_like!(Bold, Bold::new(), children),
        InlineSpec::Italic(children) => build_span_like!(Italic, Italic::new(), children),
        InlineSpec::Underline(children) => build_span_like!(Underline, Underline::new(), children),
        InlineSpec::Span(children) => build_span_like!(Span, Span::new(), children),
        InlineSpec::Hyperlink { uri, children } => {
            let mut handle = Hyperlink::new();
            if let Some(uri) = uri {
                // A false return only means the URI was rejected; the hyperlink
                // (and its children) are still valid flow content.
                let _ = handle.set_navigate_uri(uri);
            }
            let kids = match handle.inlines() {
                Some(mut col) => build_into(&mut col, children),
                None => Vec::new(),
            };
            BuiltInline::Hyperlink(handle, kids)
        }
        InlineSpec::Decorated {
            decoration,
            children,
        } => {
            let handle = Span::new();
            // A false return only means the type rejected the property; the span
            // (and its children) are still valid flow content.
            let _ = handle.set_text_decorations(*decoration);
            let kids = match handle.inlines() {
                Some(mut col) => build_into(&mut col, children),
                None => Vec::new(),
            };
            BuiltInline::Decorated(handle, kids)
        }
        InlineSpec::UiContainer { child_xaml } => {
            let mut handle = InlineUIContainer::new();
            let child = FrameworkElement::parse(child_xaml);
            match &child {
                Some(element) => {
                    if !handle.set_child(element) {
                        warn!("NoesisInlines: InlineUIContainer child is not a UIElement; skipped");
                    }
                }
                None => warn!(
                    "NoesisInlines: InlineUIContainer child XAML failed to parse: {child_xaml:?}",
                ),
            }
            BuiltInline::UiContainer(handle, child)
        }
    }
}

fn flatten_into(tree: &[BuiltInline], out: &mut String) {
    for node in tree {
        match node {
            BuiltInline::Run(h) => {
                if let Some(text) = h.text() {
                    out.push_str(&text);
                }
            }
            BuiltInline::LineBreak(_) | BuiltInline::UiContainer(_, _) => {}
            BuiltInline::Bold(_, kids)
            | BuiltInline::Italic(_, kids)
            | BuiltInline::Underline(_, kids)
            | BuiltInline::Span(_, kids)
            | BuiltInline::Hyperlink(_, kids)
            | BuiltInline::Decorated(_, kids) => flatten_into(kids, out),
        }
    }
}

fn collect_uris(tree: &[BuiltInline], out: &mut Vec<String>) {
    for node in tree {
        match node {
            BuiltInline::Hyperlink(h, kids) => {
                if let Some(uri) = h.navigate_uri() {
                    out.push(uri);
                }
                collect_uris(kids, out);
            }
            BuiltInline::Bold(_, kids)
            | BuiltInline::Italic(_, kids)
            | BuiltInline::Underline(_, kids)
            | BuiltInline::Span(_, kids)
            | BuiltInline::Decorated(_, kids) => collect_uris(kids, out),
            BuiltInline::Run(_) | BuiltInline::LineBreak(_) | BuiltInline::UiContainer(_, _) => {}
        }
    }
}

/// Collect each [`BuiltInline::Decorated`] span's *live* [`TextDecorations`],
/// depth-first, re-reading from the Noesis object (not the spec).
fn collect_decorations(tree: &[BuiltInline], out: &mut Vec<TextDecorations>) {
    for node in tree {
        match node {
            BuiltInline::Decorated(h, kids) => {
                if let Some(d) = h.text_decorations() {
                    out.push(d);
                }
                collect_decorations(kids, out);
            }
            BuiltInline::Bold(_, kids)
            | BuiltInline::Italic(_, kids)
            | BuiltInline::Underline(_, kids)
            | BuiltInline::Span(_, kids)
            | BuiltInline::Hyperlink(_, kids) => collect_decorations(kids, out),
            BuiltInline::Run(_) | BuiltInline::LineBreak(_) | BuiltInline::UiContainer(_, _) => {}
        }
    }
}

/// Count [`BuiltInline::UiContainer`]s whose *live* `Child` is present and
/// matches the element the bridge hosted, by pointer identity (depth-first).
fn count_hosted_ui(tree: &[BuiltInline], out: &mut usize) {
    for node in tree {
        match node {
            BuiltInline::UiContainer(container, child) => {
                let live = container.child_raw();
                if !live.is_null() && child.as_ref().is_some_and(|c| c.raw() == live) {
                    *out += 1;
                }
            }
            BuiltInline::Bold(_, kids)
            | BuiltInline::Italic(_, kids)
            | BuiltInline::Underline(_, kids)
            | BuiltInline::Span(_, kids)
            | BuiltInline::Hyperlink(_, kids)
            | BuiltInline::Decorated(_, kids) => count_hosted_ui(kids, out),
            BuiltInline::Run(_) | BuiltInline::LineBreak(_) => {}
        }
    }
}

/// Compute the [`InlinesReadback`] for a `TextBlock` from its live `collection`
/// and the handle `tree` the bridge built for it (empty for an un-bridged name).
pub(crate) fn readback(tree: &[BuiltInline], collection: &InlineCollection) -> InlinesReadback {
    let count = collection.count();
    let mut text = String::new();
    flatten_into(tree, &mut text);
    let mut hyperlink_uris = Vec::new();
    collect_uris(tree, &mut hyperlink_uris);
    let mut decorations = Vec::new();
    collect_decorations(tree, &mut decorations);
    let mut hosted_ui = 0;
    count_hosted_ui(tree, &mut hosted_ui);
    // Every built top-level inline must sit at its expected index in the *live*
    // collection: this is the bluff-killer (a no-op apply leaves count 0, and the
    // identity check is vacuously true only because `tree` is empty too).
    let matched = tree.len() == count
        && tree
            .iter()
            .enumerate()
            .all(|(i, node)| collection.get_raw(i) == node.raw());
    InlinesReadback {
        count,
        text,
        matched,
        hyperlink_uris,
        decorations,
        hosted_ui,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Read-back (observation)
// ─────────────────────────────────────────────────────────────────────────────

/// A snapshot of a `TextBlock`'s live inline structure, read back after a
/// [`NoesisInlines`] apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlinesReadback {
    /// Number of *top-level* inlines in the live `TextBlock.Inlines`.
    pub count: usize,
    /// The depth-first concatenation of every `Run`'s live text (no separators;
    /// `LineBreak`s contribute nothing). Read from the live Noesis `Run` objects.
    pub text: String,
    /// Whether every top-level inline the bridge built is present, by pointer
    /// identity, at its expected index in the live collection (and the counts
    /// match). Proves the built inlines really are this `TextBlock`'s content.
    pub matched: bool,
    /// Each `Hyperlink`'s live `NavigateUri`, depth-first.
    pub hyperlink_uris: Vec<String>,
    /// Each decorated span's live [`TextDecorations`], depth-first. Read from the
    /// live Noesis `Span` (not echoed from the spec).
    pub decorations: Vec<TextDecorations>,
    /// Number of `InlineUIContainer`s whose live `Child` is present and matches
    /// the element the bridge hosted, by pointer identity. Proves the embedded
    /// `UIElement` really is this container's child.
    pub hosted_ui: usize,
}

// ─────────────────────────────────────────────────────────────────────────────
// Component / message
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view formatted-text bridge. Attach to a [`NoesisView`](crate::NoesisView)
/// entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisInlines {
    /// Desired inline tree per element `x:Name`. Fully re-applied whenever this
    /// component changes: the target `TextBlock`'s `Inlines` is cleared and
    /// repopulated from the spec (see the module's re-apply semantics).
    pub set: HashMap<String, Vec<InlineSpec>>,
    /// Element `x:Name`s whose live inline structure to observe. A change vs. the
    /// previous frame emits a [`NoesisInlinesChanged`]; the first poll after a
    /// name is added always reports.
    pub watch: Vec<String>,
}

impl NoesisInlines {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name`'s inline content.
    #[must_use]
    pub fn set(
        mut self,
        name: impl Into<String>,
        inlines: impl IntoIterator<Item = InlineSpec>,
    ) -> Self {
        self.set.insert(name.into(), inlines.into_iter().collect());
        self
    }

    /// Builder: observe these elements' inline structure.
    #[must_use]
    pub fn watching(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.watch.extend(names.into_iter().map(Into::into));
        self
    }
}

/// Emitted when a watched `TextBlock`'s inline structure differs from the
/// previous frame. Read with `MessageReader<NoesisInlinesChanged>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisInlinesChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element changed.
    pub view: Entity,
    /// `x:Name` of the `TextBlock`.
    pub name: String,
    /// The current live inline structure.
    pub value: InlinesReadback,
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems / plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile every view's [`NoesisInlines`]: apply the desired inline content
/// when the component changed, then poll its watch list and emit
/// [`NoesisInlinesChanged`] for each structure that moved.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_inlines_bridge(
    views: Query<(Entity, Ref<NoesisInlines>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut changed: MessageWriter<NoesisInlinesChanged>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, inlines) in &views {
        if inlines.is_changed() {
            state.apply_inlines_for(entity, &inlines.set);
        }
        for (name, value) in state.poll_inlines_reads_for(entity, &inlines.watch) {
            changed.write(NoesisInlinesChanged {
                view: entity,
                name,
                value,
            });
        }
    }
}

/// Wires the per-view formatted-text bridge. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisInlinesPlugin;

impl Plugin for NoesisInlinesPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisInlinesChanged>()
            .add_systems(PostUpdate, sync_inlines_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_set_and_watch() {
        let c = NoesisInlines::new()
            .set("Body", [InlineSpec::run("Hi"), InlineSpec::line_break()])
            .set("Title", [InlineSpec::bold([InlineSpec::run("X")])])
            .watching(["Body", "Title"]);
        assert_eq!(
            c.set.get("Body"),
            Some(&vec![InlineSpec::Run("Hi".into()), InlineSpec::LineBreak]),
        );
        assert_eq!(
            c.set.get("Title"),
            Some(&vec![InlineSpec::Bold(vec![InlineSpec::Run("X".into())])]),
        );
        assert_eq!(c.watch, vec!["Body".to_string(), "Title".to_string()]);
    }

    #[test]
    fn decorated_and_ui_container_specs() {
        assert_eq!(
            InlineSpec::decorated(TextDecorations::Strikethrough, [InlineSpec::run("x")]),
            InlineSpec::Decorated {
                decoration: TextDecorations::Strikethrough,
                children: vec![InlineSpec::Run("x".into())],
            },
        );
        assert_eq!(
            InlineSpec::ui_container("<Rectangle/>"),
            InlineSpec::UiContainer {
                child_xaml: "<Rectangle/>".into(),
            },
        );
    }

    #[test]
    fn hyperlink_spec_carries_uri() {
        let h = InlineSpec::hyperlink("https://noesisengine.com/", [InlineSpec::run("click")]);
        assert_eq!(
            h,
            InlineSpec::Hyperlink {
                uri: Some("https://noesisengine.com/".into()),
                children: vec![InlineSpec::Run("click".into())],
            }
        );
    }
}
