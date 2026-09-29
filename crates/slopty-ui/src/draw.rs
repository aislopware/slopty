//! Building a view's elements from another entity's state: read, never written.
//!
//! GPUI draws a view again only when something it read changed, and an entity updated while
//! the window draws counts as written: every view that read it is built again. The chrome and
//! the strip are views of their own that draw the workspace's state, so they read it rather
//! than update it. A [`Draw`] stands in for the workspace's `Context` while they do: it reads
//! as the [`App`] does, hands out listeners bound to the workspace as `Context::listener`
//! does, and keeps back whatever the build would change in another entity (a terminal's zoom,
//! a face's width) until the read is over, when the view drawing applies it
//! ([`build`]).

use std::cell::RefCell;

use gpui::{AnyElement, App, Context, Empty, IntoElement as _, WeakEntity, Window};

/// Work a build keeps back until it no longer holds the entity it reads.
type Later = Box<dyn FnOnce(&mut Window, &mut App)>;

/// The context a view's elements are built in when the view is drawn by another one: `this`
/// read, `app` shared.
pub(crate) struct Draw<'a, T> {
    app: &'a App,
    this: WeakEntity<T>,
    later: RefCell<Vec<Later>>,
}

impl<'a, T: 'static> Draw<'a, T> {
    /// Building from `this` as `app` holds it.
    #[must_use]
    pub(crate) fn new(app: &'a App, this: WeakEntity<T>) -> Self {
        Self { app, this, later: RefCell::new(Vec::new()) }
    }

    /// The entity read.
    #[must_use]
    pub(crate) fn weak_entity(&self) -> WeakEntity<T> {
        self.this.clone()
    }

    /// A handler that updates the entity read, as `Context::listener` makes one.
    pub(crate) fn listener<E: ?Sized>(
        &self,
        listener: impl Fn(&mut T, &E, &mut Window, &mut Context<T>) + 'static,
    ) -> impl Fn(&E, &mut Window, &mut App) + 'static {
        let this = self.this.clone();
        move |event: &E, window: &mut Window, cx: &mut App| {
            // Gone means the window is closing: nothing to handle.
            let _gone = this.update(cx, |view, cx| listener(view, event, window, cx));
        }
    }

    /// A builder of elements the framework calls later, as a list builds its rows, that reads
    /// the entity then, as `Context::processor` makes one that updates it.
    pub(crate) fn processor<E>(
        &self,
        f: impl Fn(&T, E, &mut Window, &Draw<'_, T>) -> AnyElement + 'static,
    ) -> impl Fn(E, &mut Window, &mut App) -> AnyElement + 'static {
        let this = self.this.clone();
        move |arg, window, cx| build(&this, window, cx, |view, window, cx| f(view, arg, window, cx))
    }

    /// Do `f` once the build has let go of what it reads.
    pub(crate) fn later(&self, f: impl FnOnce(&mut Window, &mut App) + 'static) {
        self.later.borrow_mut().push(Box::new(f));
    }

    /// What the build kept back, in the order it asked.
    #[must_use]
    fn into_later(self) -> Vec<Later> {
        self.later.into_inner()
    }
}

impl<T> std::ops::Deref for Draw<'_, T> {
    type Target = App;

    fn deref(&self) -> &App {
        self.app
    }
}

/// Elements built from `this` as it is now, read and never written, and then what the build
/// kept back done. Nothing once `this` is gone.
pub(crate) fn build<T: 'static>(
    this: &WeakEntity<T>,
    window: &mut Window,
    cx: &mut App,
    f: impl FnOnce(&T, &mut Window, &Draw<'_, T>) -> AnyElement,
) -> AnyElement {
    let Some(entity) = this.upgrade() else { return Empty.into_any_element() };
    let (built, later) = {
        let draw = Draw::new(cx, this.clone());
        let built = f(entity.read(cx), window, &draw);
        (built, draw.into_later())
    };
    for f in later {
        f(window, cx);
    }
    built
}
