// Vendored from `egui_extras` 0.27.2 (`src/datepicker`, MIT OR Apache-2.0),
// with every user-visible string routed through `tr()`: the crate hardcodes
// English month names, weekday heads and the Save/Cancel buttons, which put
// English text into a Russian window. The widget logic is unchanged.

use super::popup::DatePickerPopup;
use chrono::NaiveDate;
use egui::{Area, Button, Frame, InnerResponse, Key, Order, RichText, Ui, Widget};

#[derive(Default, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct DatePickerButtonState {
    pub picker_visible: bool,
}

/// Shows a date, and will open a date picker popup when clicked.
pub struct DatePickerButton<'a> {
    selection: &'a mut NaiveDate,
    id_source: Option<&'a str>,
    combo_boxes: bool,
    arrows: bool,
    calendar: bool,
    calendar_week: bool,
    show_icon: bool,
    format: String,
    highlight_weekends: bool,
    /// When set, the button picks a period with two clicks instead of one date.
    range_end: Option<&'a mut NaiveDate>,
}

impl<'a> DatePickerButton<'a> {
    pub fn new(selection: &'a mut NaiveDate) -> Self {
        Self {
            selection,
            id_source: None,
            combo_boxes: true,
            arrows: true,
            calendar: true,
            calendar_week: true,
            show_icon: true,
            format: "%Y-%m-%d".to_owned(),
            highlight_weekends: true,
            range_end: None,
        }
    }

    /// Pick a period: the first click sets its start, the second its end.
    #[inline]
    pub fn range(mut self, end: &'a mut NaiveDate) -> Self {
        self.range_end = Some(end);
        self
    }

    /// Add id source.
    /// Must be set if multiple date picker buttons are in the same Ui.
    #[inline]
    pub fn id_source(mut self, id_source: &'a str) -> Self {
        self.id_source = Some(id_source);
        self
    }

    /// Show combo boxes in date picker popup. (Default: true)
    #[inline]
    pub fn combo_boxes(mut self, combo_boxes: bool) -> Self {
        self.combo_boxes = combo_boxes;
        self
    }

    /// Show arrows in date picker popup. (Default: true)
    #[inline]
    pub fn arrows(mut self, arrows: bool) -> Self {
        self.arrows = arrows;
        self
    }

    /// Show calendar in date picker popup. (Default: true)
    #[inline]
    pub fn calendar(mut self, calendar: bool) -> Self {
        self.calendar = calendar;
        self
    }

    /// Show calendar week in date picker popup. (Default: true)
    #[inline]
    pub fn calendar_week(mut self, week: bool) -> Self {
        self.calendar_week = week;
        self
    }

    /// Show the calendar icon on the button. (Default: true)
    #[inline]
    pub fn show_icon(mut self, show_icon: bool) -> Self {
        self.show_icon = show_icon;
        self
    }

    /// Change the format shown on the button. (Default: %Y-%m-%d)
    /// See [`chrono::format::strftime`] for valid formats.
    #[inline]
    pub fn format(mut self, format: impl Into<String>) -> Self {
        self.format = format.into();
        self
    }

    /// Highlight weekend days. (Default: true)
    #[inline]
    pub fn highlight_weekends(mut self, highlight_weekends: bool) -> Self {
        self.highlight_weekends = highlight_weekends;
        self
    }
}

/// Open a picker without a click: a script cannot press a button.
pub fn open_picker(ui: &mut egui::Ui, id_source: &str) {
    let id = ui.make_persistent_id(Some(id_source));
    ui.data_mut(|data| {
        data.insert_persisted(
            id,
            DatePickerButtonState {
                picker_visible: true,
            },
        );
    });
}

impl<'a> Widget for DatePickerButton<'a> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let id = ui.make_persistent_id(self.id_source);
        let mut button_state = ui
            .data_mut(|data| data.get_persisted::<DatePickerButtonState>(id))
            .unwrap_or_default();

        // A period is shown as a period: the two ends side by side, so the chosen
        // span stays visible after the calendar closes.
        let span = match self.range_end.as_deref() {
            Some(end) if end != self.selection => Some(format!(
                "{} – {}",
                self.selection.format(&self.format),
                end.format(&self.format)
            )),
            _ => None,
        };
        let label = span.unwrap_or_else(|| self.selection.format(&self.format).to_string());
        let mut text = if self.show_icon {
            RichText::new(format!("{label} 📆"))
        } else {
            RichText::new(label)
        };
        let visuals = ui.visuals().widgets.open;
        if button_state.picker_visible {
            text = text.color(visuals.text_color());
        }
        let mut button = Button::new(text);
        if button_state.picker_visible {
            button = button.fill(visuals.weak_bg_fill).stroke(visuals.bg_stroke);
        }
        let mut button_response = ui.add(button);
        if button_response.clicked() {
            button_state.picker_visible = true;
            ui.data_mut(|data| data.insert_persisted(id, button_state.clone()));
        }

        if button_state.picker_visible {
            // 333 was the crate's width for English labels; "Сохранить" and
            // "Неделя" need more, and a cramped popup pushed the buttons onto the
            // day grid.
            let width = 360.0;
            let mut pos = button_response.rect.left_bottom();
            let width_with_padding = width
                + ui.style().spacing.item_spacing.x
                + ui.style().spacing.window_margin.left
                + ui.style().spacing.window_margin.right;
            if pos.x + width_with_padding > ui.clip_rect().right() {
                pos.x = button_response.rect.right() - width_with_padding;
            }

            // Check to make sure the calendar never is displayed out of window
            pos.x = pos.x.max(ui.style().spacing.window_margin.left);

            //TODO(elwerene): Better positioning

            let InnerResponse {
                inner: saved,
                response: area_response,
            } = Area::new(ui.make_persistent_id(self.id_source))
                .order(Order::Foreground)
                .fixed_pos(pos)
                .constrain_to(ui.ctx().screen_rect())
                .show(ui.ctx(), |ui| {
                    let frame = Frame::popup(ui.style());
                    frame
                        .show(ui, |ui| {
                            ui.set_min_width(width);
                            ui.set_max_width(width);

                            DatePickerPopup {
                                end: self.range_end,
                                selection: self.selection,
                                button_id: id,
                                combo_boxes: self.combo_boxes,
                                arrows: self.arrows,
                                calendar: self.calendar,
                                calendar_week: self.calendar_week,
                                highlight_weekends: self.highlight_weekends,
                            }
                            .draw(ui)
                        })
                        .inner
                });

            if saved {
                button_response.mark_changed();
            }

            if !button_response.clicked()
                && (ui.input(|i| i.key_pressed(Key::Escape)) || area_response.clicked_elsewhere())
            {
                button_state.picker_visible = false;
                ui.data_mut(|data| data.insert_persisted(id, button_state));
            }
        }

        button_response
    }
}
