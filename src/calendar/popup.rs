// Vendored from `egui_extras` 0.27.2 (`src/datepicker`, MIT OR Apache-2.0),
// with every user-visible string routed through `tr()`: the crate hardcodes
// English month names, weekday heads and the Save/Cancel buttons, which put
// English text into a Russian window. The widget logic is unchanged.

use chrono::{Datelike, NaiveDate, Weekday};

use crate::i18n::tr;
use egui::{Align, Button, Color32, ComboBox, Direction, Id, Layout, RichText, Ui, Vec2};

use super::{button::DatePickerButtonState, month_data};

use egui_extras::{Column, Size, StripBuilder, TableBuilder};

#[derive(Default, Clone, serde::Deserialize, serde::Serialize)]
pub(super) struct DatePickerPopupState {
    year: i32,
    month: u32,
    day: u32,
    setup: bool,
    /// The first day of a range, as (year, month, day): the second click closes
    /// the span. Stored as numbers because this state lives in egui's memory.
    #[serde(default)]
    pub(super) pending: Option<(i32, u32, u32)>,
    /// The span chosen in this calendar. It stays here until Save, so the person
    /// sees what they picked and can start another span with a third click.
    #[serde(default)]
    pub(super) span: Option<((i32, u32, u32), (i32, u32, u32))>,
}

impl DatePickerPopupState {
    /// The chosen span as dates, if one has been picked.
    pub(super) fn span_dates(&self) -> Option<(NaiveDate, NaiveDate)> {
        let ((y1, m1, d1), (y2, m2, d2)) = self.span?;
        Some((
            NaiveDate::from_ymd_opt(y1, m1, d1)?,
            NaiveDate::from_ymd_opt(y2, m2, d2)?,
        ))
    }
}

impl DatePickerPopupState {
    fn last_day_of_month(&self) -> u32 {
        let date: NaiveDate =
            NaiveDate::from_ymd_opt(self.year, self.month, 1).expect("Could not create NaiveDate");
        date.with_day(31)
            .map(|_| 31)
            .or_else(|| date.with_day(30).map(|_| 30))
            .or_else(|| date.with_day(29).map(|_| 29))
            .unwrap_or(28)
    }
}

pub(crate) struct DatePickerPopup<'a> {
    /// The first day of the range; the second click writes this pair.
    pub end: Option<&'a mut NaiveDate>,
    pub selection: &'a mut NaiveDate,
    pub button_id: Id,
    pub combo_boxes: bool,
    pub arrows: bool,
    pub calendar: bool,
    pub calendar_week: bool,
    pub highlight_weekends: bool,
}

impl<'a> DatePickerPopup<'a> {
    /// Returns `true` if user pressed `Save` button.
    pub fn draw(&mut self, ui: &mut Ui) -> bool {
        let id = ui.make_persistent_id("date_picker");
        let today = super::today();
        let mut popup_state = ui
            .data_mut(|data| data.get_persisted::<DatePickerPopupState>(id))
            .unwrap_or_default();
        if !popup_state.setup {
            popup_state.year = self.selection.year();
            popup_state.month = self.selection.month();
            popup_state.day = self.selection.day();
            popup_state.setup = true;
            // A fresh calendar starts from the current range and with no span
            // waiting to be confirmed.
            popup_state.pending = None;
            popup_state.span = None;
            ui.data_mut(|data| data.insert_persisted(id, popup_state.clone()));
        }

        let weeks = month_data(popup_state.year, popup_state.month);
        let (mut close, mut saved) = (false, false);
        let height = 20.0;
        let spacing = 2.0;
        ui.spacing_mut().item_spacing = Vec2::splat(spacing);
        StripBuilder::new(ui)
            .clip(false)
            .sizes(
                Size::exact(height),
                match (self.combo_boxes, self.arrows) {
                    (true, true) => 2,
                    (true, false) | (false, true) => 1,
                    (false, false) => 0,
                },
            )
            .sizes(
                Size::exact((spacing + height) * (weeks.len() + 1) as f32),
                self.calendar as usize,
            )
            .size(Size::exact(height + 10.0))
            .vertical(|mut strip| {
                if self.combo_boxes {
                    strip.strip(|builder| {
                        builder.sizes(Size::remainder(), 3).horizontal(|mut strip| {
                            strip.cell(|ui| {
                                ComboBox::from_id_source("date_picker_year")
                                    .selected_text(popup_state.year.to_string())
                                    .show_ui(ui, |ui| {
                                        for year in today.year() - 100..today.year() + 10 {
                                            if ui
                                                .selectable_value(
                                                    &mut popup_state.year,
                                                    year,
                                                    year.to_string(),
                                                )
                                                .changed()
                                            {
                                                popup_state.day = popup_state
                                                    .day
                                                    .min(popup_state.last_day_of_month());
                                                ui.memory_mut(|mem| {
                                                    mem.data
                                                        .insert_persisted(id, popup_state.clone());
                                                });
                                            }
                                        }
                                    });
                            });
                            strip.cell(|ui| {
                                ComboBox::from_id_source("date_picker_month")
                                    .selected_text(month_name(popup_state.month))
                                    .show_ui(ui, |ui| {
                                        for month in 1..=12 {
                                            if ui
                                                .selectable_value(
                                                    &mut popup_state.month,
                                                    month,
                                                    month_name(month),
                                                )
                                                .changed()
                                            {
                                                popup_state.day = popup_state
                                                    .day
                                                    .min(popup_state.last_day_of_month());
                                                ui.memory_mut(|mem| {
                                                    mem.data
                                                        .insert_persisted(id, popup_state.clone());
                                                });
                                            }
                                        }
                                    });
                            });
                            strip.cell(|ui| {
                                ComboBox::from_id_source("date_picker_day")
                                    .selected_text(popup_state.day.to_string())
                                    .show_ui(ui, |ui| {
                                        for day in 1..=popup_state.last_day_of_month() {
                                            if ui
                                                .selectable_value(
                                                    &mut popup_state.day,
                                                    day,
                                                    day.to_string(),
                                                )
                                                .changed()
                                            {
                                                ui.memory_mut(|mem| {
                                                    mem.data
                                                        .insert_persisted(id, popup_state.clone());
                                                });
                                            }
                                        }
                                    });
                            });
                        });
                    });
                }

                if self.arrows {
                    strip.strip(|builder| {
                        builder.sizes(Size::remainder(), 6).horizontal(|mut strip| {
                            strip.cell(|ui| {
                                ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
                                    if ui
                                        .button("<<<")
                                        .on_hover_text(tr("Subtract one year"))
                                        .clicked()
                                    {
                                        popup_state.year -= 1;
                                        popup_state.day =
                                            popup_state.day.min(popup_state.last_day_of_month());
                                        ui.data_mut(|data| {
                                            data.insert_persisted(id, popup_state.clone());
                                        });
                                    }
                                });
                            });
                            strip.cell(|ui| {
                                ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
                                    if ui
                                        .button("<<")
                                        .on_hover_text(tr("Subtract one month"))
                                        .clicked()
                                    {
                                        popup_state.month -= 1;
                                        if popup_state.month == 0 {
                                            popup_state.month = 12;
                                            popup_state.year -= 1;
                                        }
                                        popup_state.day =
                                            popup_state.day.min(popup_state.last_day_of_month());
                                        ui.data_mut(|data| {
                                            data.insert_persisted(id, popup_state.clone());
                                        });
                                    }
                                });
                            });
                            strip.cell(|ui| {
                                ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
                                    if ui.button("<").on_hover_text(tr("Subtract one day")).clicked() {
                                        popup_state.day -= 1;
                                        if popup_state.day == 0 {
                                            popup_state.month -= 1;
                                            if popup_state.month == 0 {
                                                popup_state.year -= 1;
                                                popup_state.month = 12;
                                            }
                                            popup_state.day = popup_state.last_day_of_month();
                                        }
                                        ui.data_mut(|data| {
                                            data.insert_persisted(id, popup_state.clone());
                                        });
                                    }
                                });
                            });
                            strip.cell(|ui| {
                                ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
                                    if ui.button(">").on_hover_text(tr("Add one day")).clicked() {
                                        popup_state.day += 1;
                                        if popup_state.day > popup_state.last_day_of_month() {
                                            popup_state.day = 1;
                                            popup_state.month += 1;
                                            if popup_state.month > 12 {
                                                popup_state.month = 1;
                                                popup_state.year += 1;
                                            }
                                        }
                                        ui.data_mut(|data| {
                                            data.insert_persisted(id, popup_state.clone());
                                        });
                                    }
                                });
                            });
                            strip.cell(|ui| {
                                ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
                                    if ui.button(">>").on_hover_text(tr("Add one month")).clicked() {
                                        popup_state.month += 1;
                                        if popup_state.month > 12 {
                                            popup_state.month = 1;
                                            popup_state.year += 1;
                                        }
                                        popup_state.day =
                                            popup_state.day.min(popup_state.last_day_of_month());
                                        ui.data_mut(|data| {
                                            data.insert_persisted(id, popup_state.clone());
                                        });
                                    }
                                });
                            });
                            strip.cell(|ui| {
                                ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
                                    if ui.button(">>>").on_hover_text(tr("Add one year")).clicked() {
                                        popup_state.year += 1;
                                        popup_state.day =
                                            popup_state.day.min(popup_state.last_day_of_month());
                                        ui.data_mut(|data| {
                                            data.insert_persisted(id, popup_state.clone());
                                        });
                                    }
                                });
                            });
                        });
                    });
                }

                if self.calendar {
                    strip.cell(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::new(1.0, 2.0);
                        let mut table = TableBuilder::new(ui).vscroll(false);
                        if self.calendar_week {
                            // Wide enough for "Неделя" on one line; the seven day
                            // cells share what is left.
                            table = table.column(Column::exact(48.0));
                        }
                        table
                            .columns(Column::remainder(), 7)
                            .header(height, |mut header| {
                                if self.calendar_week {
                                    header.col(|ui| {
                                        ui.with_layout(
                                            Layout::centered_and_justified(Direction::TopDown),
                                            |ui| {
                                                ui.add(
                                                    egui::Label::new(tr("Week")).wrap(false),
                                                );
                                            },
                                        );
                                    });
                                }

                                for name in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].map(tr) {
                                    header.col(|ui| {
                                        ui.with_layout(
                                            Layout::centered_and_justified(Direction::TopDown),
                                            |ui| {
                                                ui.label(name);
                                            },
                                        );
                                    });
                                }
                            })
                            .body(|mut body| {
                                for week in weeks {
                                    body.row(height, |mut row| {
                                        if self.calendar_week {
                                            row.col(|ui| {
                                                ui.label(week.number.to_string());
                                            });
                                        }
                                        for day in week.days {
                                            row.col(|ui| {
                                                ui.with_layout(
                                                    Layout::top_down_justified(Align::Center),
                                                    |ui| {
                                                        // The span being chosen lives in the
                                                        // calendar until Save: the two ends are
                                                        // dark, the days between them are tinted,
                                                        // and another click starts a new span.
                                                        let span = popup_state.span_dates();
                                                        let in_range = match span {
                                                            Some((from, to)) => {
                                                                from <= day && to >= day
                                                            }
                                                            None => match self.end.as_deref() {
                                                                Some(end) => {
                                                                    *self.selection <= day
                                                                        && *end >= day
                                                                }
                                                                None => false,
                                                            },
                                                        };
                                                        let is_edge = match span {
                                                            Some((from, to)) => {
                                                                day == from || day == to
                                                            }
                                                            None => false,
                                                        };
                                                        let is_pending = popup_state.pending
                                                            == Some((day.year(), day.month(), day.day()));
                                                        let fill_color = if is_edge || is_pending {
                                                            ui.visuals().selection.bg_fill
                                                        } else if in_range {
                                                            ui.visuals()
                                                                .selection
                                                                .bg_fill
                                                                .linear_multiply(0.45)
                                                        } else if (day.weekday() == Weekday::Sat
                                                            || day.weekday() == Weekday::Sun)
                                                            && self.highlight_weekends
                                                        {
                                                            if ui.visuals().dark_mode {
                                                                Color32::DARK_RED
                                                            } else {
                                                                Color32::LIGHT_RED
                                                            }
                                                        } else {
                                                            ui.visuals().extreme_bg_color
                                                        };

                                                        let mut text_color = ui
                                                            .visuals()
                                                            .widgets
                                                            .inactive
                                                            .text_color();

                                                        if day.month() != popup_state.month {
                                                            text_color =
                                                                text_color.linear_multiply(0.5);
                                                        };

                                                        let button_response = ui.add(
                                                            Button::new(
                                                                RichText::new(
                                                                    day.day().to_string(),
                                                                )
                                                                .color(text_color),
                                                            )
                                                            .fill(fill_color),
                                                        );

                                                        if day == today {
                                                            // Encircle today's date
                                                            let stroke = ui
                                                                .visuals()
                                                                .widgets
                                                                .inactive
                                                                .fg_stroke;
                                                            ui.painter().circle_stroke(
                                                                button_response.rect.center(),
                                                                8.0,
                                                                stroke,
                                                            );
                                                        }

                                                        if button_response.clicked() {
                                                            popup_state.year = day.year();
                                                            popup_state.month = day.month();
                                                            popup_state.day = day.day();
                                                            if self.end.is_some() {
                                                                // The span stays in the calendar:
                                                                // nothing is applied until Save, and
                                                                // a third click starts another span.
                                                                let picked =
                                                                    pick_day(&mut popup_state, day);
                                                                crate::log::log(&format!(
                                                                    "calendar click {day} picked={picked:?}"
                                                                ));
                                                            } else {
                                                                // Single-date mode: the click is
                                                                // the value, Save writes it.
                                                                *self.selection = day;
                                                            }
                                                            ui.data_mut(|data| {
                                                                data.insert_persisted(
                                                                    id,
                                                                    popup_state.clone(),
                                                                );
                                                            });
                                                        }
                                                    },
                                                );
                                            });
                                        }
                                    });
                                }
                            });
                    });
                }

                strip.strip(|builder| {
                    builder.sizes(Size::remainder(), 2).horizontal(|mut strip| {
                        strip.empty();
                        strip.cell(|ui| {
                            // Both buttons sit at the right edge, and each one is
                            // wider than its own label so the text has air on the
                            // sides. Save is the outermost: it is the one the
                            // person reaches for.
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if bottom_button(ui, &tr("Save")).clicked() {
                                    // Save is what applies the choice: the span
                                    // picked in the grid, or a single day when no
                                    // span was picked.
                                    match popup_state.span_dates() {
                                        Some((from, to)) => match self.end.as_deref_mut() {
                                            Some(end) => {
                                                *self.selection = from;
                                                *end = to;
                                            }
                                            None => *self.selection = from,
                                        },
                                        None => {
                                            *self.selection = NaiveDate::from_ymd_opt(
                                                popup_state.year,
                                                popup_state.month,
                                                popup_state.day,
                                            )
                                            .expect("Could not create NaiveDate");
                                        }
                                    }
                                    saved = true;
                                    close = true;
                                }
                                if bottom_button(ui, &tr("Cancel")).clicked() {
                                    close = true;
                                }
                            });
                        });
                    });
                });
            });

        if close {
            popup_state.setup = false;
            ui.data_mut(|data| {
                data.insert_persisted(id, popup_state);
                data.get_persisted_mut_or_default::<DatePickerButtonState>(self.button_id)
                    .picker_visible = false;
            });
        }

        saved && close
    }
}

/// What one click on a day does to a period: the first click marks the start,
/// the second closes the span in order and reports it. `None` means the person
/// still has to pick the other end.
pub(super) fn pick_day(state: &mut DatePickerPopupState, day: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    let numbers = (day.year(), day.month(), day.day());
    match state.pending {
        None => {
            // First click: a one-day span, waiting for the other end.
            state.pending = Some(numbers);
            state.span = Some((numbers, numbers));
            None
        }
        Some((year, month, dom)) => {
            let first = NaiveDate::from_ymd_opt(year, month, dom).unwrap_or(day);
            state.pending = None;
            let span = (first.min(day), first.max(day));
            state.span = Some((
                (span.0.year(), span.0.month(), span.0.day()),
                (span.1.year(), span.1.month(), span.1.day()),
            ));
            Some(span)
        }
    }
}

/// A bottom-row button: at least 32 points wider than its own label, so a word
/// like "Сохранить" keeps air on both sides instead of touching the border.
fn bottom_button(ui: &mut Ui, label: &str) -> egui::Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, ui.visuals().text_color())
        .size()
        .x;
    let size = Vec2::new(text_width + 32.0, ui.spacing().interact_size.y);
    ui.add_sized(size, Button::new(label))
}

/// The month's name in the interface language.
fn month_name(i: u32) -> String {
    match i {
        1 => tr("January"),
        2 => tr("February"),
        3 => tr("March"),
        4 => tr("April"),
        5 => tr("May"),
        6 => tr("June"),
        7 => tr("July"),
        8 => tr("August"),
        9 => tr("September"),
        10 => tr("October"),
        11 => tr("November"),
        12 => tr("December"),
        _ => panic!("Unknown month: {i}"),
    }
}
