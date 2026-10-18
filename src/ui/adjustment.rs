use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;

use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, Label, Orientation};

use crate::calibration::CalibrationCommand;
use crate::monado::ResetTarget;
use crate::ui::state::SharedState;

use super::window::ToastManager;

#[derive(Clone)]
pub(super) struct AdjustmentEditor {
    pub widget: GtkBox,
    pub button: Button,
    controls: gtk4::Grid,
    values: Vec<Label>,
    target: Rc<Cell<Option<u32>>>,
    target_label: Label,
    save_button: Button,
    ready: Rc<Cell<bool>>,
    saving: Rc<Cell<bool>>,
}

impl AdjustmentEditor {
    pub fn new(cmd_tx: &Rc<mpsc::Sender<CalibrationCommand>>, toasts: &ToastManager) -> Self {
        let widget = GtkBox::new(Orientation::Vertical, 10);
        widget.set_halign(Align::Center);
        widget.set_visible(false);

        let target_label = Label::new(None);
        target_label.add_css_class("heading");
        widget.append(&target_label);

        let controls = gtk4::Grid::new();
        controls.set_row_spacing(6);
        controls.set_column_spacing(6);
        let target = Rc::new(Cell::new(None));
        let mut values = Vec::new();
        let deltas = [-10.0, -1.0, -0.1, -0.01, 0.01, 0.1, 1.0, 10.0];
        for (axis, name) in [
            "X (m)",
            "Y (m)",
            "Z (m)",
            "Pitch (°)",
            "Yaw (°)",
            "Roll (°)",
        ]
        .into_iter()
        .enumerate()
        {
            let row = i32::try_from(axis).unwrap();
            let label = Label::new(Some(name));
            label.set_xalign(0.0);
            controls.attach(&label, 0, row, 1, 1);
            let value = Label::new(Some("—"));
            value.set_width_chars(10);
            value.set_xalign(1.0);
            value.add_css_class("monospace");
            controls.attach(&value, 1, row, 1, 1);
            values.push(value);
            for (index, delta) in deltas.into_iter().enumerate() {
                let button = Button::with_label(&format!("{delta:+}"));
                button.set_tooltip_text(Some(&format!("{name}: {delta:+}")));
                controls.attach(&button, i32::try_from(index).unwrap() + 2, row, 1, 1);
                let cmd_tx = Rc::clone(cmd_tx);
                let target = Rc::clone(&target);
                let toasts = toasts.clone();
                button.connect_clicked(move |_| {
                    let Some(target) = target.get() else {
                        return;
                    };
                    if let Err(error) = cmd_tx.send(CalibrationCommand::Adjust {
                        target: ResetTarget::TrackingOrigin(target),
                        axis,
                        delta,
                    }) {
                        toasts.show(&format!("Failed to adjust offset: {error}"));
                    }
                });
            }
        }
        widget.append(&controls);
        let hint = Label::new(Some(
            "Changes apply immediately. Done closes the editor without saving.",
        ));
        hint.add_css_class("caption");
        hint.add_css_class("dim-label");
        widget.append(&hint);

        let save_button = Button::with_label("Save");
        save_button.set_halign(Align::Center);
        save_button.add_css_class("suggested-action");
        save_button.set_tooltip_text(Some(
            "Save the adjusted Target calibration for next launch and motoc continue",
        ));
        widget.append(&save_button);
        let save_hint = Label::new(Some(
            "Save for next launch and motoc continue. Updates the saved Target calibration.",
        ));
        save_hint.set_wrap(true);
        save_hint.set_max_width_chars(70);
        save_hint.add_css_class("caption");
        save_hint.add_css_class("dim-label");
        widget.append(&save_hint);

        let button = Button::with_label("Adjust");
        button.set_width_request(90);
        button.set_tooltip_text(Some("Fine-tune the selected Target tracking origin"));
        let editor = Self {
            widget,
            button,
            controls,
            values,
            target,
            target_label,
            save_button,
            ready: Rc::new(Cell::new(false)),
            saving: Rc::new(Cell::new(false)),
        };

        editor.connect_save(cmd_tx, toasts);

        editor
    }

    fn connect_save(&self, cmd_tx: &Rc<mpsc::Sender<CalibrationCommand>>, toasts: &ToastManager) {
        let editor_for_save = self.clone();
        let cmd_tx_save = Rc::clone(cmd_tx);
        let toasts_for_save = toasts.clone();
        self.save_button.connect_clicked(move |_| {
            let editor = &editor_for_save;
            if !editor.ready.get() || editor.saving.get() {
                return;
            }
            let Some(target) = editor.target.get() else {
                return;
            };
            editor.saving.set(true);
            editor.controls.set_sensitive(false);
            editor.button.set_sensitive(false);
            editor.save_button.set_label("Saving...");
            editor.update_save_state();
            if let Err(error) = cmd_tx_save.send(CalibrationCommand::SaveAdjustment {
                target: ResetTarget::TrackingOrigin(target),
            }) {
                editor.save_finished();
                toasts_for_save.show(&format!("Failed to save adjustment: {error}"));
            }
        });
    }

    pub fn open(
        &self,
        state: &SharedState,
        cmd_tx: &mpsc::Sender<CalibrationCommand>,
        toasts: &ToastManager,
    ) -> bool {
        let state = state.borrow();
        let Some(target) = state.selected_target() else {
            toasts.show("Select a Target device before adjusting its offset");
            return false;
        };
        self.target.set(Some(target.category_index));
        self.target_label
            .set_text(&format!("Adjust Target: {}", target.category));
        self.widget.set_visible(true);
        self.button.set_label("Done");
        self.button.set_tooltip_text(Some(
            "Close the editor; unsaved changes remain applied for this session",
        ));
        self.read(cmd_tx, toasts);
        true
    }

    fn read(&self, cmd_tx: &mpsc::Sender<CalibrationCommand>, toasts: &ToastManager) {
        self.ready.set(false);
        self.controls.set_sensitive(false);
        self.update_save_state();
        for value in &self.values {
            value.set_text("—");
        }
        if let Some(target) = self.target.get()
            && let Err(error) = cmd_tx.send(CalibrationCommand::ReadAdjustment {
                target: ResetTarget::TrackingOrigin(target),
            })
        {
            toasts.show(&format!("Failed to read offset: {error}"));
        }
    }

    pub fn update(&self, target: ResetTarget, values: [f64; 6]) {
        if self.widget.is_visible()
            && self.target.get().map(ResetTarget::TrackingOrigin) == Some(target)
        {
            for (label, value) in self.values.iter().zip(values) {
                label.set_text(&format!("{value:+.3}"));
            }
            self.ready.set(true);
            self.controls.set_sensitive(!self.saving.get());
            self.update_save_state();
        }
    }

    fn update_save_state(&self) {
        self.save_button
            .set_sensitive(self.ready.get() && !self.saving.get());
    }

    pub fn is_saving(&self) -> bool {
        self.saving.get()
    }

    pub fn save_finished(&self) {
        self.saving.set(false);
        self.save_button.set_label("Save");
        self.controls.set_sensitive(self.ready.get());
        self.button.set_sensitive(true);
        self.update_save_state();
    }

    pub fn close(&self) {
        self.widget.set_visible(false);
        self.ready.set(false);
        self.save_finished();
        self.button.set_label("Adjust");
        self.button
            .set_tooltip_text(Some("Fine-tune the selected Target tracking origin"));
    }
}
