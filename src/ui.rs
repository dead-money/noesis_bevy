//! [`NoesisUi`]: ergonomic access to the single [`NoesisView`] in a one-UI app.
//!
//! The per-view bridges live as components on the view's camera entity, so a
//! system that drives them needs that entity. For an app with one UI that means
//! repeating `Single<&mut NoesisText, With<NoesisView>>` (and a separate lookup
//! for the entity itself) on every system. [`NoesisUi`] folds both into one
//! parameter:
//!
//! ```ignore
//! // Write a bridge component on the one view.
//! fn tick_score(mut ui: NoesisUi<&mut NoesisText>) {
//!     let Some(mut text) = ui.get_mut() else { return };
//!     text.write("Score", "42");
//! }
//!
//! // Just the view entity, e.g. to match a read-back message.
//! fn on_change(ui: NoesisUi, mut changed: MessageReader<NoesisTextChanged>) {
//!     let Some(view) = ui.entity() else { return };
//!     for ev in changed.read() {
//!         if ev.view == view { /* ... */ }
//!     }
//! }
//! ```
//!
//! Unlike a bare [`Single`], `NoesisUi` does not
//! skip the whole system when there isn't exactly one view: the accessors return
//! [`None`] and the system decides. It is a single-view convenience; a multi-view
//! app routes by the `view: Entity` each read-back message carries and queries
//! the bridges directly.

use bevy::ecs::query::{QueryData, QueryItem, ROQueryItem};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::render::NoesisView;

/// System parameter for the single [`NoesisView`] and, optionally, one of its
/// bridge components `D`.
///
/// Use `NoesisUi` on its own for the view [`entity`](Self::entity), or
/// `NoesisUi<&NoesisText>` / `NoesisUi<&mut NoesisText>` to also read or write a
/// bridge component on that view. `D` can be any query data, so tuples work too:
/// `NoesisUi<(&mut NoesisText, &mut NoesisVisibility)>`.
///
/// Every accessor returns [`None`] when zero or more than one [`NoesisView`]
/// exists, so the system still runs and chooses what to do. For multiple views,
/// route by the `view: Entity` each read-back message carries instead.
#[derive(SystemParam)]
pub struct NoesisUi<'w, 's, D: QueryData + 'static = ()> {
    view: Query<'w, 's, (Entity, D), With<NoesisView>>,
}

impl<'w, 's, D: QueryData + 'static> NoesisUi<'w, 's, D> {
    /// The one view's entity, or [`None`] if zero or more than one exists.
    pub fn entity(&self) -> Option<Entity> {
        self.view.single().ok().map(|(entity, _)| entity)
    }

    /// Read access to `D` on the one view, or [`None`] if zero or more than one
    /// view exists.
    pub fn get(&self) -> Option<ROQueryItem<'_, 's, D>> {
        self.view.single().ok().map(|(_, data)| data)
    }

    /// Mutable access to `D` on the one view, or [`None`] if zero or more than
    /// one view exists. This is the runtime write path: `ui.get_mut()` then call
    /// the bridge's `&mut self` setters (`write`, `show`, `set_*`, ...).
    ///
    /// The extra `IterQueryData` bound tracks Bevy 0.19's `Query::single_mut`
    /// signature; every real query data (`&mut T`, tuples, ...) satisfies it, so
    /// the read-only accessors above stay unconstrained.
    pub fn get_mut(&mut self) -> Option<QueryItem<'_, 's, D>>
    where
        D: bevy::ecs::query::IterQueryData,
    {
        self.view.single_mut().ok().map(|(_, data)| data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::NoesisView;
    use crate::text::NoesisText;

    fn write_score(mut ui: NoesisUi<&mut NoesisText>) {
        if let Some(mut text) = ui.get_mut() {
            text.write("Score", "42");
        }
    }

    fn record_entity(ui: NoesisUi, mut out: ResMut<SeenEntity>) {
        out.0 = ui.entity();
    }

    #[derive(Resource, Default)]
    struct SeenEntity(Option<Entity>);

    #[test]
    fn resolves_single_view_entity_and_component() {
        let mut app = App::new();
        app.init_resource::<SeenEntity>();
        let view = app
            .world_mut()
            .spawn((
                NoesisView {
                    xaml_uri: "x.xaml".to_string(),
                    ..default()
                },
                NoesisText::new(),
            ))
            .id();
        app.add_systems(Update, (write_score, record_entity));
        app.update();

        assert_eq!(app.world().resource::<SeenEntity>().0, Some(view));
        let text = app.world().entity(view).get::<NoesisText>().unwrap();
        assert_eq!(text.set.get("Score").map(String::as_str), Some("42"));
    }

    #[test]
    fn none_when_no_view() {
        let mut app = App::new();
        app.init_resource::<SeenEntity>();
        app.add_systems(Update, record_entity);
        app.update();
        assert_eq!(app.world().resource::<SeenEntity>().0, None);
    }
}
