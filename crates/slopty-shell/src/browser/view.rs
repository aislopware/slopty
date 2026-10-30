// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3d2f1e6), see THIRD_PARTY_NOTICES.md
use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Focusable as _, IntoElement, KeyDownEvent, MouseButton, Render, Window,
    div, px,
};

use super::{BrowserEvent, BrowserSurface};
use crate::motion::HoverFade as _;
use crate::settings::widgets::ActionHint as _;
use crate::theme::Theme;
use crate::{icons, surface_chrome};

/// A toolbar control: its id, its name, what it draws, whether it can act, and the action whose
/// key its tooltip names.
struct Control {
    id: &'static str,
    label: &'static str,
    glyph: &'static str,
    enabled: bool,
    action: Option<Box<dyn gpui::Action>>,
}

fn button(
    Control { id, label, glyph, enabled, action }: Control,
    focus: &gpui::FocusHandle,
    theme: &Theme,
    cx: &Context<BrowserSurface>,
) -> gpui::Stateful<gpui::Div> {
    // Match Files/History chrome, including focus-preserving mouse-down and
    // tooltips. Disabled controls have neither a pointer cursor nor a handler.
    // The tooltip names the key the browser binds (⌘[ ⌘] ⇧⌘R), resolved in its context.
    match action {
        Some(action) => {
            crate::files::toolbar_button_keyed(id, label, action, focus.clone(), theme, cx)
        }
        None => crate::files::toolbar_button(id, label, theme),
    }
    // A control that cannot act is no keyboard stop (M8).
    .when(!enabled, |el| {
        el.cursor_default().tab_stop(false).opacity(crate::theme::DISABLED_OPACITY)
    })
    .child(icons::icon(glyph).size(px(surface_chrome::ICON_SIZE)).text_color(theme.text_muted))
}

impl BrowserSurface {
    pub(super) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address_focused = self.address.focus_handle(cx).is_focused(window);
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;
        let primary = if cfg!(target_os = "macos") { mods.platform } else { mods.control };
        if address_focused && !primary && !mods.alt {
            match key {
                "enter" if !mods.shift => self.submit(window, cx),
                "escape" => {
                    let url = self.page.url.clone().unwrap_or_default();
                    self.address.update(cx, |input, cx| input.set_text(url, cx));
                    self.address_edited = false;
                    self.validation = None;
                    window.focus(&self.focus, cx);
                }
                _ => return,
            }
        } else {
            return;
        }
        cx.stop_propagation();
    }

    fn preview_body(&self, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let snapshot = &self.previews;
        let subtitle = if snapshot.remote { "Running on your device" } else { "Running locally" };
        let mut content = div()
            .w_full()
            .max_w(px(280.0))
            .flex_shrink_0()
            .my_auto()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .mb(px(4.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(subtitle),
            );
        for service in &snapshot.services {
            let url = service.url();
            let row_url = url.clone();
            let border_strong = theme.border_strong;
            let label = if snapshot.remote {
                format!("{} · localhost:{}", service.device_name, service.port)
            } else {
                format!("localhost:{}", service.port)
            };
            content = content.child(
                div()
                    .id(gpui::SharedString::from(format!("preview-row-{}", service.id)))
                    .w_full()
                    .h(px(56.0))
                    .px(px(14.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    // The row's wash fades in with the pointer, as the pane's rows do
                    // (M23); its border still steps at once.
                    .map(|el| {
                        let fade = format!("preview-row-{}-fade", service.id);
                        el.cursor_pointer()
                            .bg(crate::motion::hover_blend(
                                &fade,
                                crate::theme::ink(0.02),
                                crate::theme::ink(0.05),
                            ))
                            .hover_fade(fade)
                            .hover(move |style| style.border_color(border_strong))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.navigate(&row_url, window, cx);
                            }))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .truncate()
                                    .child(service.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text_muted)
                                    .truncate()
                                    .child(label),
                            ),
                    )
                    .child(
                        div()
                            .id(gpui::SharedString::from(format!("open-preview-{}", service.id)))
                            .h(px(28.0))
                            .px(px(6.0))
                            .flex_shrink_0()
                            .rounded(px(6.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .role(gpui::Role::Button)
                            .aria_label(format!("Open {} preview", service.name))
                            .map(|el| {
                                // The row's keyboard stop; the row itself is the
                                // pointer's larger target for the same press (M8).
                                let fade = format!("open-preview-{}-fade", service.id);
                                el.cursor_pointer()
                                    .map(|el| surface_chrome::key_stop(el, theme))
                                    .bg(crate::motion::hover_blend(
                                        &fade,
                                        crate::theme::wash(0.0),
                                        crate::theme::control_hover(),
                                    ))
                                    .hover_fade(fade)
                                    .active(|style| style.bg(crate::theme::pressed_wash()))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.navigate(&url, window, cx);
                                    }))
                            })
                            .child("Open"),
                    ),
            );
        }
        if snapshot.services.is_empty() {
            let message = if self.previews_loading {
                "Looking for dev servers…"
            } else if snapshot.remote {
                "Start a dev server in this project on your other device. Its preview will appear here when that device is online."
            } else {
                "Start a dev server in this project. It will appear here automatically, ready to open."
            };
            content = content.child(
                div()
                    .p(px(16.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .text_size(crate::typography::ui_rems(12.0))
                    .line_height(px(19.0))
                    .text_color(theme.text_muted)
                    .child(message),
            );
        }
        content = content.child(
            div()
                .id("preview-enter-address")
                .mt(px(8.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .role(gpui::Role::Button)
                .aria_label("Enter a website address")
                .rounded(px(4.0))
                // A keyboard stop with the shell's ring (M8).
                .map(|el| surface_chrome::key_stop(el, theme))
                .on_click(cx.listener(|this, _, window, cx| this.focus_address(window, cx)))
                .child("Or enter a website address"),
        );
        div()
            .id("browser-previews")
            .size_full()
            .overflow_y_scroll()
            .p(px(16.0))
            .flex()
            .flex_col()
            .items_center()
            .child(content)
            .into_any_element()
    }

    fn empty_body(&self, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        if self.page.url.is_none() && self.previews_task.is_some() {
            return self.preview_body(theme, cx);
        }
        let external = !cfg!(target_os = "macos");
        let has_error = self.page.error.is_some();
        let starting = false;
        let title = if has_error {
            "Couldn't load this page"
        } else if starting {
            "Starting browser…"
        } else if external && self.page.url.is_some() {
            "Opened in your browser"
        } else {
            "Preview your work"
        };
        let description = if let Some(error) = &self.page.error {
            error.clone()
        } else if starting {
            "You can keep working while your page opens.".into()
        } else if external {
            "Open a website or local app in your default browser. Embedded browsing is available on macOS.".into()
        } else {
            "Preview your local app or keep a website beside your conversation.".into()
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(300.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .size(px(44.0))
                            .rounded(px(12.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.surface_raised.opacity(0.5))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                icons::icon(icons::GLOBE)
                                    .size(px(22.0))
                                    .text_color(theme.text_muted),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_size(crate::typography::ui_rems(14.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_center()
                            .text_size(crate::typography::ui_rems(12.0))
                            .line_height(px(19.0))
                            .text_color(theme.text_muted)
                            .child(description),
                    )
                    .child(
                        div()
                            .mt(px(6.0))
                            .id("browser-empty-action")
                            .h(px(28.0))
                            .px(px(10.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.surface_raised)
                            .cursor_pointer()
                            .role(gpui::Role::Button)
                            .aria_label(if has_error { "Try again" } else { "Enter an address" })
                            // The hover fades a control step over the raised fill (M23).
                            .bg(crate::motion::hover_blend(
                                "browser-empty-action-fade",
                                theme.surface_raised,
                                theme.surface_raised.blend(crate::theme::control_hover()),
                            ))
                            .hover_fade("browser-empty-action-fade")
                            .active({
                                let pressed =
                                    theme.surface_raised.blend(crate::theme::pressed_wash());
                                move |style| style.bg(pressed)
                            })
                            // A keyboard stop with the shell's ring (M8).
                            .map(|el| surface_chrome::key_stop(el, theme))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if has_error {
                                    this.reload(cx);
                                } else {
                                    this.focus_address(window, cx);
                                }
                            }))
                            .child(if has_error { "Try again" } else { "Enter an address" })
                            .when(!has_error, |el| {
                                el.child(
                                    div()
                                        .text_size(crate::typography::ui_rems(11.0))
                                        .text_color(theme.text_faint)
                                        .child(if cfg!(target_os = "macos") {
                                            "⌘L"
                                        } else {
                                            "Ctrl L"
                                        }),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl Render for BrowserSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        #[cfg(target_os = "macos")]
        let persistence_notice = self.context.data.persistence_notice();
        #[cfg(not(target_os = "macos"))]
        let persistence_notice: Option<&str> = None;
        let focused = self.address.focus_handle(cx).is_focused(window);
        let external = !cfg!(target_os = "macos");
        let has_page = self.page.url.is_some();
        let back = button(
            Control {
                id: "browser-back",
                label: "Back",
                glyph: icons::ARROW_LEFT,
                enabled: self.page.can_back,
                action: Some(Box::new(super::Back)),
            },
            &self.focus,
            &theme,
            cx,
        )
        .when(self.page.can_back, |el| {
            el.on_click(cx.listener(|this, _, _, _| this.history(false)))
        });
        let forward = button(
            Control {
                id: "browser-forward",
                label: "Forward",
                glyph: icons::ARROW_RIGHT,
                enabled: self.page.can_forward,
                action: Some(Box::new(super::Forward)),
            },
            &self.focus,
            &theme,
            cx,
        )
        .when(self.page.can_forward, |el| {
            el.on_click(cx.listener(|this, _, _, _| this.history(true)))
        });
        let reload = button(
            Control {
                id: "browser-reload",
                label: "Reload page",
                glyph: icons::REFRESH,
                enabled: has_page,
                action: Some(Box::new(super::Reload)),
            },
            &self.focus,
            &theme,
            cx,
        )
        .when(has_page, |el| el.on_click(cx.listener(|this, _, _, cx| this.reload(cx))));
        let address = surface_chrome::input()
            .id("browser-address")
            .when(self.validation.is_some(), |el| el.border_1().border_color(theme.danger))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    #[cfg(target_os = "macos")]
                    if let Some(native) = &this.native {
                        native.focus_chrome();
                    }
                    #[cfg(not(target_os = "macos"))]
                    let _: &mut BrowserSurface = this;
                }),
            )
            .child(
                icons::icon(icons::GLOBE).size(px(12.0)).flex_none().text_color(theme.text_faint),
            )
            .child(
                div().flex_1().min_w_0().h(px(16.0)).overflow_hidden().child(self.address.clone()),
            )
            .when(focused || self.address_edited, |el| {
                // An 18 px mark in a 24 px target (M25); its tooltip names ⏎, which the
                // address field takes.
                el.child(
                    div()
                        .id("browser-go")
                        .size(px(24.0))
                        .m(px(-3.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .role(gpui::Role::Button)
                        .hover_fade("browser-go-fade")
                        // A keyboard stop with the shell's ring (M8).
                        .map(|el| surface_chrome::key_stop(el, &theme))
                        .on_click(cx.listener(|this, _, w, cx| this.submit(w, cx)))
                        .action_hint(
                            "Go to address",
                            crate::settings::widgets::Keys::Fixed("enter"),
                            cx,
                        )
                        .child(
                            div()
                                .size(px(18.0))
                                .rounded(px(Theme::RADIUS_SM))
                                .flex()
                                .items_center()
                                .justify_center()
                                // The mark's wash fades in with the pointer over the
                                // whole button (M23).
                                .bg(crate::motion::hover_blend(
                                    "browser-go-fade",
                                    crate::theme::wash(0.0),
                                    crate::theme::control_hover(),
                                ))
                                .child(
                                    icons::icon(icons::RETURN)
                                        .size(px(12.0))
                                        .text_color(theme.text_muted),
                                ),
                        ),
                )
            });
        let open = button(
            Control {
                id: "browser-external",
                label: "Open in default browser",
                glyph: icons::ARROW_UP_RIGHT,
                enabled: has_page,
                action: None,
            },
            &self.focus,
            &theme,
            cx,
        )
        .when(has_page, |el| el.on_click(cx.listener(|this, _, _, cx| this.open_external(cx))));
        let toolbar = surface_chrome::toolbar(&theme)
            .when(!external, |el| el.child(back).child(forward).child(reload))
            .child(address)
            .child(open);

        let body = div()
            .id("browser-page")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            // Empty tabs share the shell's glass, like the sidebar tab picker.
            // Keep an opaque backing while native web content is loading.
            .when(has_page, |body| body.bg(theme.bg));
        #[cfg(target_os = "macos")]
        let body = if let Some(native) = &self.native {
            if self.page.error.is_some() {
                body.child(self.empty_body(&theme, cx))
            } else {
                // gpui-fast natives under one layer (docs/composition.md): the
                // element places the page, cuts its hole under the Files panel and routes
                // hits and focus, so no present-time sync is needed.
                let native = native.handle();
                native.borrow_mut().sync();
                let host = native.borrow().native_host();
                body.child(gpui::composition::native_view(&host).absolute().inset_0())
            }
        } else {
            body.child(self.empty_body(&theme, cx))
        };
        #[cfg(not(target_os = "macos"))]
        let body = body.child(self.empty_body(&theme, cx));

        let remote_loopback = self.remote
            && self
                .page
                .url
                .as_deref()
                .and_then(|s| url::Url::parse(s).ok())
                .is_some_and(|u| super::model::loopback(&u));
        div().id("browser-surface").size_full().flex().flex_col().track_focus(&self.focus)
            .key_context("Browser").on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(|_, _: &gpui::KeyUpEvent, _, _| {}))
            .on_action(cx.listener(|this, _: &super::Reload, _, cx| this.reload(cx)))
            .on_action(cx.listener(|this, _: &super::FocusAddress, w, cx| this.focus_address(w, cx)))
            .on_action(cx.listener(|_, _: &super::NewTab, _, cx| cx.emit(BrowserEvent::NewTab(None))))
            .on_action(cx.listener(|_, _: &super::CloseTab, _, cx| cx.emit(BrowserEvent::Close)))
            .on_action(cx.listener(|this, _: &super::Back, _, _| this.history(false)))
            .on_action(cx.listener(|this, _: &super::Forward, _, _| this.history(true)))
            // Mouse side buttons drive the shell's route history everywhere
            // else; over the browser they are inert.
            .on_mouse_down(MouseButton::Navigate(gpui::NavigationDirection::Back), |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Navigate(gpui::NavigationDirection::Forward), |_, _, cx| cx.stop_propagation())
            .child(toolbar)
            .when_some(persistence_notice, |el, message| el.child(div().px(px(12.0)).py(px(8.0))
                .text_size(crate::typography::ui_rems(11.0)).text_color(theme.text_muted).child(message.to_owned())))
            .when_some(self.validation.clone(), |el, message| el.child(div().px(px(12.0)).py(px(8.0)).text_size(crate::typography::ui_rems(11.0)).text_color(theme.danger).child(message)))
            .when(remote_loopback, |el| el.child(div().px(px(12.0)).py(px(8.0)).border_b_1().border_color(theme.border)
                .text_size(crate::typography::ui_rems(11.0)).text_color(theme.text_muted)
                .child("Localhost opens on this device. Open a detected preview from a new tab to reach your other device.")))
            .child(body)
            .when(external, |el| el.child(div().h(px(26.0)).px(px(10.0)).flex().items_center().gap(px(Theme::SPACE_SNUG)).border_t_1().border_color(theme.border)
                .text_size(crate::typography::ui_rems(11.0)).text_color(theme.text_faint)
                .child(icons::icon(icons::ARROW_UP_RIGHT).size(px(12.0))).child("Opens in your default browser")))
    }
}
