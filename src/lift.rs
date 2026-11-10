//! A wrapper widget that scales its content up around its centre and draws it
//! above its neighbours, with a drop shadow, so it reads as lifted.
//!
//! Adapted from iced's `Float` widget, which libcosmic's `Theme` cannot style
//! (it does not implement `Float`'s style catalog). This version takes the
//! shadow directly and leaves out `Float`'s translation option.

use libcosmic::iced::advanced::widget::{Tree, tree};
use libcosmic::iced::advanced::{
    Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer, renderer::Renderer as _,
};
use libcosmic::iced::border;
use libcosmic::iced::{Event, Length, Rectangle, Shadow, Size, Transformation, Vector};
use libcosmic::{Element, Renderer, Theme};

/// Content drawn at `scale` (1.0 = normal size) with an optional drop shadow.
///
/// The layout is that of the unscaled content, so neighbours do not move. At a
/// scale above 1.0 the content is drawn as an overlay, on top of everything
/// around it, and pointer positions are mapped back into the content so hover
/// and clicks keep working.
///
/// ```ignore
/// let card = Lift::new(card).scale(1.05).shadow(shadow, radius);
/// ```
pub struct Lift<'a, Message> {
    content: Element<'a, Message>,
    scale: f32,
    shadow: Shadow,
    shadow_radius: border::Radius,
}

impl<'a, Message> Lift<'a, Message> {
    /// Wrap `content`, at its normal size and without a shadow.
    pub fn new(content: impl Into<Element<'a, Message>>) -> Self {
        Self {
            content: content.into(),
            scale: 1.0,
            shadow: Shadow::default(),
            shadow_radius: border::Radius::default(),
        }
    }

    /// Set the scale the content is drawn at.
    #[must_use]
    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// Set the drop shadow, drawn behind the content with corners rounded by `radius`.
    #[must_use]
    pub fn shadow(mut self, shadow: Shadow, radius: impl Into<border::Radius>) -> Self {
        self.shadow = shadow;
        self.shadow_radius = radius.into();
        self
    }

    fn is_lifted(&self) -> bool {
        self.scale > 1.0
    }

    /// Draw the shadow behind `bounds`, if it is visible at all.
    fn draw_shadow(&self, renderer: &mut Renderer, bounds: Rectangle) {
        if self.shadow.color.a <= 0.0 {
            return;
        }
        renderer.fill_quad(
            renderer::Quad {
                bounds: bounds.shrink(1.0),
                shadow: self.shadow,
                border: border::rounded(self.shadow_radius),
                snap: false,
            },
            self.shadow.color,
        );
    }
}

impl<Message> Widget<Message, Theme, Renderer> for Lift<'_, Message> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&mut self, tree: &mut Tree) {
        self.content.as_widget_mut().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // While lifted, the overlay handles events instead.
        if self.is_lifted() {
            return;
        }
        self.content.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        // While lifted, the overlay draws the content instead.
        if self.is_lifted() {
            return;
        }
        self.draw_shadow(renderer, layout.bounds());
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.is_lifted() {
            return mouse::Interaction::None;
        }
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn libcosmic::iced::advanced::widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        offset: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        if !self.is_lifted() {
            return self
                .content
                .as_widget_mut()
                .overlay(tree, layout, renderer, viewport, offset);
        }

        // Scale around the centre of the content's own bounds.
        let bounds = layout.bounds();
        let centre = Vector::new(
            bounds.x + bounds.width / 2.0,
            bounds.y + bounds.height / 2.0,
        );
        let transformation = Transformation::translate(centre.x + offset.x, centre.y + offset.y)
            * Transformation::scale(self.scale)
            * Transformation::translate(-centre.x, -centre.y);

        Some(overlay::Element::new(Box::new(LiftOverlay {
            lift: self,
            tree,
            layout,
            viewport: *viewport,
            transformation,
        })))
    }
}

impl<'a, Message: 'a> From<Lift<'a, Message>> for Element<'a, Message> {
    fn from(lift: Lift<'a, Message>) -> Self {
        Element::new(lift)
    }
}

/// The lifted content, drawn scaled on top of the rest of the window.
struct LiftOverlay<'a, 'b, Message> {
    lift: &'a mut Lift<'b, Message>,
    tree: &'a mut Tree,
    layout: Layout<'a>,
    viewport: Rectangle,
    transformation: Transformation,
}

impl<Message> overlay::Overlay<Message, Theme, Renderer> for LiftOverlay<'_, '_, Message> {
    fn layout(&mut self, _renderer: &Renderer, _bounds: Size) -> layout::Node {
        let bounds = self.layout.bounds() * self.transformation;
        layout::Node::new(bounds.size()).move_to(bounds.position())
    }

    fn update(
        &mut self,
        event: &Event,
        _layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        let inverse = self.transformation.inverse();
        self.lift.content.as_widget_mut().update(
            self.tree,
            event,
            self.layout,
            cursor * inverse,
            renderer,
            clipboard,
            shell,
            &(self.viewport * inverse),
        );
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        _layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let bounds = self.layout.bounds();
        let inverse = self.transformation.inverse();

        renderer.with_layer(self.viewport, |renderer| {
            renderer.with_transformation(self.transformation, |renderer| {
                self.lift.draw_shadow(renderer, bounds);
                self.lift.content.as_widget().draw(
                    self.tree,
                    renderer,
                    theme,
                    style,
                    self.layout,
                    cursor * inverse,
                    &(self.viewport * inverse),
                );
            });
        });
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if !cursor.is_over(layout.bounds()) {
            return mouse::Interaction::None;
        }
        let inverse = self.transformation.inverse();
        self.lift.content.as_widget().mouse_interaction(
            self.tree,
            self.layout,
            cursor * inverse,
            &(self.viewport * inverse),
            renderer,
        )
    }

    /// Larger lifts stack above smaller ones, e.g. a cell settling back while
    /// its neighbour lifts.
    fn index(&self) -> f32 {
        self.lift.scale * 0.5
    }

    fn overlay<'c>(
        &'c mut self,
        _layout: Layout<'c>,
        renderer: &Renderer,
    ) -> Option<overlay::Element<'c, Message, Theme, Renderer>> {
        // Nested overlays (e.g. a tooltip inside the content) follow the lift.
        self.lift.content.as_widget_mut().overlay(
            self.tree,
            self.layout,
            renderer,
            &(self.viewport * self.transformation.inverse()),
            self.transformation.translation(),
        )
    }
}
