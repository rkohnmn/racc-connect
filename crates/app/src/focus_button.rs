//! Keyboard-focus behavior for app controls.
use super::{design::tokens, Message};
use iced::advanced::{
    mouse::{self, Cursor},
    overlay, renderer,
    widget::{
        self,
        tree::{self, Tree},
        Id, Operation,
    },
    Clipboard, Layout, Shell, Widget,
};
use iced::advanced::{widget::operation::Focusable, Renderer as _};
use iced::{Border, Color, Element, Event, Rectangle, Theme};

/// A button that participates in iced keyboard focus traversal.
pub(super) struct FocusableButton<'a> {
    inner: iced::widget::Button<'a, Message>,
    action: Option<Message>,
    id: Id,
}

impl<'a> FocusableButton<'a> {
    /// Wraps a styled button and its activation message with stable focus state.
    pub(super) fn wrap(
        inner: iced::widget::Button<'a, Message>,
        action: Option<Message>,
        id: Id,
    ) -> Self {
        Self { inner, action, id }
    }
}

#[derive(Debug, Default)]
struct FocusState {
    focused: bool,
}

impl widget::operation::Focusable for FocusState {
    fn is_focused(&self) -> bool {
        self.focused
    }

    fn focus(&mut self) {
        self.focused = true;
    }

    fn unfocus(&mut self) {
        self.focused = false;
    }
}

impl<'a> Widget<Message, Theme, iced::Renderer> for FocusableButton<'a> {
    fn size(&self) -> iced::Size<iced::Length> {
        self.inner.size()
    }

    fn size_hint(&self) -> iced::Size<iced::Length> {
        self.inner.size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<FocusState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(FocusState::default())
    }

    fn children(&self) -> Vec<Tree> {
        let inner: &dyn Widget<Message, Theme, iced::Renderer> = &self.inner;
        vec![Tree::new(inner)]
    }

    fn diff(&self, tree: &mut Tree) {
        let inner: &dyn Widget<Message, Theme, iced::Renderer> = &self.inner;
        tree.diff_children(&[inner]);
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &iced::advanced::layout::Limits,
    ) -> iced::advanced::layout::Node {
        self.inner.layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        let state = tree.state.downcast_mut::<FocusState>();
        if self.action.is_some() {
            operation.focusable(Some(&self.id), layout.bounds(), state);
        } else {
            state.unfocus();
        }
        self.inner
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if self.action.is_some() && !shell.is_event_captured() {
            if matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            ) && cursor.is_over(layout.bounds())
            {
                shell.publish(Message::FocusWidget(self.id.clone()));
            }

            let focused = tree.state.downcast_ref::<FocusState>().is_focused();
            let activate = matches!(
                event,
                Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(
                        iced::keyboard::key::Named::Enter | iced::keyboard::key::Named::Space
                    ),
                    repeat: false,
                    ..
                })
            );
            if focused && activate {
                if let Some(action) = self.action.clone() {
                    shell.publish(action);
                    shell.capture_event();
                    return;
                }
            }
        }

        self.inner.update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        self.inner.draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );

        if tree.state.downcast_ref::<FocusState>().is_focused() {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: Border {
                        color: tokens::ACCENT,
                        width: tokens::FOCUS_RING_WIDTH,
                        radius: tokens::RADIUS_MEDIUM.into(),
                    },
                    ..Default::default()
                },
                Color::TRANSPARENT,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.inner
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: iced::Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.inner.overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a> From<FocusableButton<'a>> for Element<'a, Message> {
    fn from(button: FocusableButton<'a>) -> Self {
        Element::new(button)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_state_tracks_focus_and_release() {
        let mut state = FocusState::default();
        assert!(!state.is_focused());
        state.focus();
        assert!(state.is_focused());
        state.unfocus();
        assert!(!state.is_focused());
    }
}
