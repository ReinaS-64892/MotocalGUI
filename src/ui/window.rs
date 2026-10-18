use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, CssProvider, Label, Orientation, Overlay};
use libadwaita as adw;
use libadwaita::prelude::*;

use std::cell::{Cell, RefCell};

use futures_util::StreamExt;

use crate::calibration::{CalibrationCommand, CalibrationMessage};
use crate::monado::ResetTarget;
use crate::ui::device_list::{DeviceList, PopoverState};
use crate::ui::state::{SharedState, create_shared_state};
use crate::xr::xr_event_loop;

// Dismisses previous toast before showing a new one.
#[derive(Clone)]
struct ToastManager {
    overlay: adw::ToastOverlay,
    current_toast: Rc<RefCell<Option<adw::Toast>>>,
}

impl ToastManager {
    fn new(overlay: adw::ToastOverlay) -> Self {
        Self {
            overlay,
            current_toast: Rc::new(RefCell::new(None)),
        }
    }

    fn show(&self, message: &str) {
        if let Some(prev) = self.current_toast.borrow_mut().take() {
            prev.dismiss();
        }

        let toast = adw::Toast::new(message);
        self.current_toast.borrow_mut().replace(toast.clone());
        self.overlay.add_toast(toast);
    }
}

fn play_sound(sound_id: &str) {
    let _ = std::process::Command::new("canberra-gtk-play")
        .arg("-i")
        .arg(sound_id)
        // Keep VR calibration cues separate from desktop notification sounds,
        // which may be muted automatically during screen capture.
        .arg("--property=media.role=game")
        .arg("--property=application.name=MotocalGUI")
        .arg("--property=application.id=io.github.ReinaS_64892.MotocalGUI")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn restore_idle_visuals(background_box: &GtkBox, countdown_label: &Label, progress_fill: &GtkBox) {
    background_box.remove_css_class("calibration-bg-active");
    countdown_label.set_visible(false);
    progress_fill.set_visible(false);
    progress_fill.remove_css_class("progress-fill-active");
    progress_fill.set_size_request(0, -1);
}

fn update_progress_fill(fill: &GtkBox, window: &adw::ApplicationWindow, fraction: f64) {
    let fill_width = (f64::from(window.width()) * fraction).round() as i32;
    fill.set_size_request(fill_width, -1);
}

fn build_help_dialog(state: &SharedState, calibrate_action: bool) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::builder()
        .heading("Calibration Instructions")
        .build();

    let extra_box = GtkBox::new(Orientation::Vertical, 16);

    let instructions = Label::new(None);
    instructions.set_markup(
        "1. Grip both selected devices <b>firmly together</b> \u{2014} they must \
         not shift or twist relative to each other from start to finish\n\n\
         2. Wait for the countdown beeps to finish, then \
         <b>slowly move and rotate both devices together</b>, turning and \
         tilting them in different directions\n\n\
         3. Keep both devices tracked and the motion <b>slow and steady</b>. \
         Avoid sudden movements and continue until the progress bar completes \
         or you hear the chime",
    );
    instructions.set_xalign(0.0);
    instructions.set_wrap(true);
    instructions.set_max_width_chars(56);
    extra_box.append(&instructions);

    let tip = Label::new(Some(
        "Tip: Open a device dropdown and move a device to see which one it is",
    ));
    tip.set_halign(Align::Center);
    tip.add_css_class("dim-label");
    tip.add_css_class("caption");
    extra_box.append(&tip);

    let pref_box = GtkBox::new(Orientation::Horizontal, 12);
    pref_box.set_halign(Align::Center);
    let pref_label = Label::new(Some("Don't show before calibrating"));
    let pref_switch = gtk4::Switch::new();
    pref_switch.set_active(state.borrow().hide_calibration_help());

    let state_for_switch = Rc::clone(state);
    pref_switch.connect_active_notify(move |switch| {
        state_for_switch
            .borrow_mut()
            .set_hide_calibration_help(switch.is_active());
    });

    pref_box.append(&pref_label);
    pref_box.append(&pref_switch);
    extra_box.append(&pref_box);

    dialog.set_extra_child(Some(&extra_box));

    if calibrate_action {
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("calibrate", "Calibrate");
        dialog.set_response_appearance("calibrate", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("calibrate"));
        dialog.set_close_response("cancel");
    } else {
        dialog.add_response("got_it", "Got it");
    }

    dialog
}

#[allow(clippy::too_many_lines, clippy::items_after_statements)]
pub fn build_ui(app: &adw::Application) {
    let style_manager = adw::StyleManager::default();
    style_manager.set_color_scheme(adw::ColorScheme::PreferDark);

    let css_provider = CssProvider::new();
    css_provider.load_from_string(
        ".calibration-bg-active { \
            background-color: rgba(53, 132, 228, 0.12); \
            transition: background-color 500ms ease; \
        } \
        .progress-fill-active { \
            background-color: rgba(53, 132, 228, 0.38); \
        } \
        .countdown-text { \
            font-size: 420px; \
            font-weight: 900; \
            font-family: Impact, sans-serif; \
            color: rgba(53, 132, 228, 0.35); \
        }",
    );
    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("Could not connect to a display"),
        &css_provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let state = create_shared_state();

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (msg_tx, msg_rx) = async_channel::bounded::<CalibrationMessage>(100);

    thread::spawn(move || {
        xr_event_loop(cmd_rx, msg_tx);
    });

    let cmd_tx = Rc::new(cmd_tx);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("MotocalGUI")
        .default_width(800)
        .default_height(630)
        .build();

    window.set_size_request(800, 630);

    let toolbar_view = adw::ToolbarView::new();

    let overlay = Overlay::new();
    window.set_content(Some(&overlay));

    let background_box = GtkBox::new(Orientation::Vertical, 0);
    background_box.set_hexpand(true);
    background_box.set_vexpand(true);
    overlay.set_child(Some(&background_box));

    let progress_fill = GtkBox::new(Orientation::Vertical, 0);
    progress_fill.set_halign(Align::Start);
    progress_fill.set_vexpand(true);
    progress_fill.set_visible(false);
    background_box.append(&progress_fill);

    let countdown_label = Label::new(None);
    countdown_label.set_halign(Align::Center);
    countdown_label.set_valign(Align::Center);
    countdown_label.add_css_class("countdown-text");
    countdown_label.set_visible(false);
    overlay.add_overlay(&countdown_label);

    overlay.add_overlay(&toolbar_view);
    overlay.set_measure_overlay(&toolbar_view, true);

    let header_bar = adw::HeaderBar::new();
    header_bar.set_title_widget(Some(&Label::new(Some("MotocalGUI"))));
    header_bar.set_decoration_layout(Some(":close"));
    toolbar_view.add_top_bar(&header_bar);

    let help_btn = Button::from_icon_name("dialog-question-symbolic");
    help_btn.set_tooltip_text(Some("Calibration Instructions"));
    header_bar.pack_end(&help_btn);

    let toast_overlay = adw::ToastOverlay::new();
    toolbar_view.set_content(Some(&toast_overlay));
    let toasts = ToastManager::new(toast_overlay.clone());

    let main_box = GtkBox::new(Orientation::Vertical, 20);
    main_box.set_margin_top(20);
    main_box.set_margin_bottom(20);
    main_box.set_margin_start(24);
    main_box.set_margin_end(24);
    toast_overlay.set_child(Some(&main_box));

    let top_row = GtkBox::new(Orientation::Horizontal, 16);
    top_row.set_halign(Align::Center);
    top_row.set_hexpand(false);
    main_box.append(&top_row);

    let source_list = DeviceList::new("Source");
    source_list.widget().set_width_request(250);
    source_list
        .widget()
        .set_tooltip_text(Some("Reference device with correct tracking"));
    top_row.append(source_list.widget());

    let target_list = DeviceList::new("Target");
    target_list.widget().set_width_request(250);
    target_list
        .widget()
        .set_tooltip_text(Some("Device to calibrate (its origin is adjusted)"));
    top_row.append(target_list.widget());

    let button_box = GtkBox::new(Orientation::Horizontal, 12);
    button_box.set_halign(Align::Center);
    main_box.append(&button_box);

    let refresh_btn = Button::from_icon_name("view-refresh-symbolic");
    refresh_btn.set_tooltip_text(Some("Refresh Devices"));
    button_box.append(&refresh_btn);

    let calibrate_btn = adw::SplitButton::new();
    calibrate_btn.set_label("Calibrate");
    calibrate_btn.add_css_class("suggested-action");
    calibrate_btn.set_width_request(110);
    calibrate_btn.set_tooltip_text(Some(
        "Align target tracking to source (dropdown: sample count)",
    ));

    let sample_count = Rc::new(Cell::new(state.borrow().sample_count()));
    let sample_popover = gtk4::Popover::new();
    let sample_list = gtk4::ListBox::new();
    sample_list.set_selection_mode(gtk4::SelectionMode::None);
    sample_list.add_css_class("boxed-list");
    sample_list.set_margin_start(6);
    sample_list.set_margin_end(6);
    sample_list.set_margin_top(6);
    sample_list.set_margin_bottom(6);

    let sample_options: &[(u32, &str)] = &[(200, "Quick"), (400, "Balanced"), (600, "Precise")];
    let check_labels: Rc<RefCell<Vec<Label>>> = Rc::new(RefCell::new(Vec::new()));
    for &(count, name) in sample_options {
        let row = gtk4::ListBoxRow::new();
        let hbox = GtkBox::new(Orientation::Horizontal, 8);
        hbox.set_margin_start(12);
        hbox.set_margin_end(12);
        hbox.set_margin_top(8);
        hbox.set_margin_bottom(8);

        let current = sample_count.get();
        let check = Label::new(if count == current {
            Some("\u{2713}")
        } else {
            None
        });
        check.set_width_request(16);
        check_labels.borrow_mut().push(check.clone());

        let label = Label::new(Some(name));
        label.set_xalign(0.0);
        label.set_hexpand(true);

        hbox.append(&check);
        hbox.append(&label);
        row.set_child(Some(&hbox));
        sample_list.append(&row);
    }

    let sample_count_for_list = Rc::clone(&sample_count);
    let sample_popover_for_list = sample_popover.clone();
    let check_labels_for_list = Rc::clone(&check_labels);
    let state_for_sample = Rc::clone(&state);
    sample_list.connect_row_activated(move |_, row| {
        let counts = [200u32, 400, 600];
        let index: usize = row.index().try_into().unwrap();
        if let Some(&count) = counts.get(index) {
            sample_count_for_list.set(count);
            state_for_sample.borrow_mut().set_sample_count(count);
            let labels = check_labels_for_list.borrow();
            for (i, label) in labels.iter().enumerate() {
                label.set_text(if i == index { "\u{2713}" } else { "" });
            }
        }
        sample_popover_for_list.popdown();
    });

    sample_popover.set_child(Some(&sample_list));
    calibrate_btn.set_popover(Some(&sample_popover));
    button_box.append(&calibrate_btn);

    let continuous_btn = gtk4::ToggleButton::with_label("Continuous");
    continuous_btn.set_tooltip_text(Some(
        "Enable before Calibrate to keep calibrating continuously; turn off to stop. \
         Keep both devices fixed together while active.",
    ));
    button_box.append(&continuous_btn);

    // Backend state updates must not enqueue another stop command.
    let syncing_continuous = Rc::new(Cell::new(false));
    let syncing_for_toggle = Rc::clone(&syncing_continuous);
    let cmd_tx_continuous = Rc::clone(&cmd_tx);
    let toasts_continuous = toasts.clone();
    continuous_btn.connect_toggled(move |btn| {
        if !syncing_for_toggle.get()
            && !btn.is_active()
            && let Err(error) = cmd_tx_continuous.send(CalibrationCommand::StopContinuous)
        {
            toasts_continuous.show(&format!("Failed to stop continuous calibration: {error}"));
        }
    });

    let recenter_btn = Button::with_label("Recenter");
    recenter_btn.set_width_request(90);
    recenter_btn.set_tooltip_text(Some("Set STAGE origin to current HMD position and heading"));
    button_box.append(&recenter_btn);

    let reset_btn = adw::SplitButton::new();
    reset_btn.set_label("Reset");
    reset_btn.add_css_class("destructive-action");
    reset_btn.set_tooltip_text(Some(
        "Reset target tracking origin (dropdown for all origins and STAGE/LOCAL)",
    ));
    let reset_popover = gtk4::Popover::new();
    let reset_list = gtk4::ListBox::new();
    reset_list.set_selection_mode(gtk4::SelectionMode::None);
    reset_list.add_css_class("boxed-list");
    reset_popover.set_child(Some(&reset_list));
    reset_btn.set_popover(Some(&reset_popover));
    button_box.append(&reset_btn);

    let status_label = Label::new(None);
    status_label.set_halign(Align::Center);
    status_label.add_css_class("dim-label");
    status_label.set_margin_top(8);
    main_box.append(&status_label);

    let battery_bar = GtkBox::new(Orientation::Horizontal, 16);
    battery_bar.set_halign(Align::Center);
    battery_bar.set_margin_top(4);
    main_box.append(&battery_bar);

    fn update_ui_from_state(
        state: &SharedState,
        source_list: &Rc<DeviceList>,
        target_list: &Rc<DeviceList>,
        status_label: &Label,
        battery_bar: &GtkBox,
    ) {
        let s = state.borrow();

        if s.is_connected() {
            status_label.set_text("Connected");
            status_label.remove_css_class("warning");
        } else {
            status_label.set_text("Waiting for Monado...");
            if !status_label.has_css_class("warning") {
                status_label.add_css_class("warning");
            }
        }

        let source_devices = s.source_devices();
        source_list.set_devices(source_devices, s.source_name());

        let target_devices = s.target_devices();
        target_list.set_devices(target_devices, s.target_name());

        update_battery_bar(battery_bar, &s);
    }

    fn update_battery_bar(bar: &GtkBox, state: &crate::ui::state::AppState) {
        while let Some(child) = bar.first_child() {
            bar.remove(&child);
        }

        let battery_list = state.battery_status_list();
        if battery_list.is_empty() {
            return;
        }

        for info in &battery_list {
            let item = GtkBox::new(Orientation::Vertical, 2);
            item.set_halign(Align::Center);

            if !info.online {
                let top = GtkBox::new(Orientation::Horizontal, 4);
                top.set_halign(Align::Center);
                let icon = gtk4::Image::from_icon_name("network-offline-symbolic");
                icon.set_pixel_size(16);
                icon.add_css_class("dim-label");
                let label = Label::new(Some(&format!("{} Offline", info.short_name)));
                label.add_css_class("caption");
                label.add_css_class("dim-label");
                top.append(&icon);
                top.append(&label);
                item.append(&top);
            } else if let Some(charge) = info.charge {
                let pct = (charge * 100.0).round().clamp(0.0, 100.0) as u32;
                let icon_name = if info.charging {
                    if pct > 75 {
                        "battery-full-charging-symbolic"
                    } else if pct > 40 {
                        "battery-good-charging-symbolic"
                    } else if pct > 15 {
                        "battery-low-charging-symbolic"
                    } else {
                        "battery-caution-charging-symbolic"
                    }
                } else if pct > 75 {
                    "battery-full-symbolic"
                } else if pct > 40 {
                    "battery-good-symbolic"
                } else if pct > 15 {
                    "battery-low-symbolic"
                } else {
                    "battery-caution-symbolic"
                };

                let top = GtkBox::new(Orientation::Horizontal, 4);
                top.set_halign(Align::Center);
                let icon = gtk4::Image::from_icon_name(icon_name);
                icon.set_pixel_size(16);
                let text = format!("{} {}%", info.short_name, pct);
                let label = Label::new(Some(&text));
                label.add_css_class("caption");
                if pct <= 15 {
                    label.add_css_class("error");
                    icon.add_css_class("error");
                } else if pct <= 40 {
                    label.add_css_class("warning");
                    icon.add_css_class("warning");
                } else {
                    label.add_css_class("dim-label");
                    icon.add_css_class("dim-label");
                }
                top.append(&icon);
                top.append(&label);
                item.append(&top);
            } else {
                let top = GtkBox::new(Orientation::Horizontal, 4);
                top.set_halign(Align::Center);
                let label = Label::new(Some(&info.short_name));
                label.add_css_class("caption");
                label.add_css_class("dim-label");
                top.append(&label);
                item.append(&top);
            }

            if !info.serial_suffix.is_empty() {
                let serial_label = Label::new(Some(&info.serial_suffix));
                serial_label.add_css_class("caption");
                serial_label.add_css_class("dim-label");
                serial_label.set_halign(Align::Center);
                item.append(&serial_label);
            }

            bar.append(&item);
        }
    }

    fn update_movement_only(
        state: &SharedState,
        source_list: &Rc<DeviceList>,
        target_list: &Rc<DeviceList>,
    ) {
        let intensities = state.borrow().movement_intensities().clone();
        source_list.update_movement(intensities);
        target_list.update_movement(state.borrow().movement_intensities().clone());
    }

    update_ui_from_state(
        &state,
        &source_list,
        &target_list,
        &status_label,
        &battery_bar,
    );

    let state_for_source = Rc::clone(&state);
    let target_list_for_source = Rc::clone(&target_list);
    source_list.connect_changed(move |device| {
        let unique_id = device.map(|d| d.unique_id().to_string());
        state_for_source.borrow_mut().set_source(unique_id);
        let s = state_for_source.borrow();
        target_list_for_source.set_devices(s.target_devices(), s.target_name());
    });

    let state_for_target = Rc::clone(&state);
    let source_list_for_target = Rc::clone(&source_list);
    target_list.connect_changed(move |device| {
        let unique_id = device.map(|d| d.unique_id().to_string());
        state_for_target.borrow_mut().set_target(unique_id);
        let s = state_for_target.borrow();
        source_list_for_target.set_devices(s.source_devices(), s.source_name());
    });

    let state_for_refresh = Rc::clone(&state);
    let source_list_for_refresh = Rc::clone(&source_list);
    let target_list_for_refresh = Rc::clone(&target_list);
    let status_for_refresh = status_label.clone();
    let battery_bar_for_refresh = battery_bar.clone();
    refresh_btn.connect_clicked(move |_| {
        state_for_refresh.borrow_mut().force_refresh();
        update_ui_from_state(
            &state_for_refresh,
            &source_list_for_refresh,
            &target_list_for_refresh,
            &status_for_refresh,
            &battery_bar_for_refresh,
        );
    });

    let state_for_monitor = Rc::clone(&state);
    let source_list_for_monitor = Rc::clone(&source_list);
    let target_list_for_monitor = Rc::clone(&target_list);
    let status_for_monitor = status_label;
    let battery_bar_for_monitor = battery_bar;
    let window_focused = Rc::new(Cell::new(true));

    window.connect_has_focus_notify({
        let wf = Rc::clone(&window_focused);
        move |w| {
            wf.set(w.has_focus());
        }
    });

    fn schedule_poll(
        state: SharedState,
        source_list: Rc<DeviceList>,
        target_list: Rc<DeviceList>,
        status_label: Label,
        battery_bar: GtkBox,
        window_focused: Rc<Cell<bool>>,
        mut last_battery_refresh: std::time::Instant,
    ) {
        let focused = window_focused.get();
        let interval_ms = if focused { 500 } else { 3000 };

        glib::timeout_add_local_once(std::time::Duration::from_millis(interval_ms), move || {
            let was_connected = state.borrow().is_connected();
            let changed = state.borrow_mut().refresh_connection();
            let battery_interval = std::time::Duration::from_secs(
                if window_focused.get() { 5 } else { 30 },
            );
            if state.borrow().is_connected()
                && (changed || last_battery_refresh.elapsed() >= battery_interval)
            {
                state.borrow_mut().refresh_batteries();
                last_battery_refresh = std::time::Instant::now();
            }
            if was_connected == state.borrow().is_connected() {
                update_battery_bar(&battery_bar, &state.borrow());
            } else {
                update_ui_from_state(
                    &state,
                    &source_list,
                    &target_list,
                    &status_label,
                    &battery_bar,
                );
            }

            schedule_poll(
                state,
                source_list,
                target_list,
                status_label,
                battery_bar,
                window_focused,
                last_battery_refresh,
            );
        });
    }

    schedule_poll(
        state_for_monitor,
        source_list_for_monitor,
        target_list_for_monitor,
        status_for_monitor,
        battery_bar_for_monitor,
        window_focused,
        std::time::Instant::now(),
    );

    let open_popovers = Rc::new(std::cell::RefCell::new(0u32));

    let cmd_tx_source_vis = Rc::clone(&cmd_tx);
    let open_popovers_source = Rc::clone(&open_popovers);
    let state_source_vis = Rc::clone(&state);
    source_list.connect_popover_visibility(move |visibility| {
        let mut count = open_popovers_source.borrow_mut();
        match visibility {
            PopoverState::Opened => {
                if *count == 0 && state_source_vis.borrow().is_connected() {
                    let _ = cmd_tx_source_vis.send(CalibrationCommand::StartMovementDetection);
                }
                *count += 1;
            }
            PopoverState::Closed => {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    let _ = cmd_tx_source_vis.send(CalibrationCommand::StopMovementDetection);
                }
            }
        }
    });

    let cmd_tx_target_vis = Rc::clone(&cmd_tx);
    let open_popovers_target = Rc::clone(&open_popovers);
    let state_target_vis = Rc::clone(&state);
    target_list.connect_popover_visibility(move |visibility| {
        let mut count = open_popovers_target.borrow_mut();
        match visibility {
            PopoverState::Opened => {
                if *count == 0 && state_target_vis.borrow().is_connected() {
                    let _ = cmd_tx_target_vis.send(CalibrationCommand::StartMovementDetection);
                }
                *count += 1;
            }
            PopoverState::Closed => {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    let _ = cmd_tx_target_vis.send(CalibrationCommand::StopMovementDetection);
                }
            }
        }
    });

    let toasts_for_calibrate = toasts.clone();
    let state_for_calibrate = Rc::clone(&state);
    let cmd_tx_calibrate = Rc::clone(&cmd_tx);
    let window_for_calibrate = window.clone();
    let sample_count_for_calibrate = Rc::clone(&sample_count);
    let continuous_for_calibrate = continuous_btn.clone();
    calibrate_btn.connect_clicked(move |btn| {
        let s = state_for_calibrate.borrow();
        let (Some(src), Some(tgt)) = (s.selected_source(), s.selected_target()) else {
            toasts_for_calibrate.show("Select both source and target devices");
            return;
        };

        let hide_help = s.hide_calibration_help();
        let source_serial = src.unique_id().to_string();
        let target_serial = tgt.unique_id().to_string();
        drop(s);

        let cmd = CalibrationCommand::StartSampled {
            source_serial,
            target_serial,
            sample_count: sample_count_for_calibrate.get(),
            continuous: continuous_for_calibrate.is_active(),
        };

        if hide_help {
            if let Err(e) = cmd_tx_calibrate.send(cmd) {
                toasts_for_calibrate.show(&format!("Failed to start calibration: {e}"));
                return;
            }
            btn.set_label("Calibrating...");
            btn.set_sensitive(false);
        } else {
            let dialog = build_help_dialog(&state_for_calibrate, true);
            let cmd_tx = Rc::clone(&cmd_tx_calibrate);
            let btn = btn.clone();
            let toasts = toasts_for_calibrate.clone();
            dialog.connect_response(None, move |_, response| {
                if response == "calibrate" {
                    if let Err(e) = cmd_tx.send(cmd.clone()) {
                        toasts.show(&format!("Failed to start calibration: {e}"));
                        return;
                    }
                    btn.set_label("Calibrating...");
                    btn.set_sensitive(false);
                }
            });
            dialog.present(Some(&window_for_calibrate));
        }
    });

    let toasts_for_reset = toasts.clone();
    let state_for_reset = Rc::clone(&state);
    let cmd_tx_reset = Rc::clone(&cmd_tx);
    reset_btn.connect_clicked(move |_| {
        let state = state_for_reset.borrow();
        let Some(target) = state.selected_target() else {
            toasts_for_reset.show("Select a target device first");
            return;
        };
        if let Err(error) = cmd_tx_reset.send(CalibrationCommand::Reset {
            target: ResetTarget::TrackingOrigin(target.category_index),
            label: target.category.clone(),
        }) {
            toasts_for_reset.show(&format!("Failed to reset: {error}"));
        }
    });

    // Keep each row's actual origin ID; IDs need not equal menu positions.
    let reset_targets = Rc::new(RefCell::new(Vec::new()));
    let targets_for_show = Rc::clone(&reset_targets);
    let state_for_reset_show = Rc::clone(&state);
    let list_for_reset_show = reset_list.clone();
    reset_popover.connect_show(move |_| {
        while let Some(row) = list_for_reset_show.first_child() {
            list_for_reset_show.remove(&row);
        }
        let state = state_for_reset_show.borrow();
        let mut targets = targets_for_show.borrow_mut();
        targets.clear();
        targets.extend(state.categories().iter().map(|category| {
            (
                category.name.clone(),
                ResetTarget::TrackingOrigin(category.index),
            )
        }));
        targets.extend([
            ("STAGE Space".into(), ResetTarget::Stage),
            ("LOCAL Space".into(), ResetTarget::Local),
        ]);
        for (name, _) in targets.iter() {
            let row = gtk4::ListBoxRow::new();
            let label = Label::new(Some(name));
            label.set_xalign(0.0);
            label.set_margin_start(12);
            label.set_margin_end(12);
            label.set_margin_top(8);
            label.set_margin_bottom(8);
            row.set_child(Some(&label));
            list_for_reset_show.append(&row);
        }
    });

    let cmd_tx_reset_list = Rc::clone(&cmd_tx);
    let toasts_for_reset_list = toasts.clone();
    reset_list.connect_row_activated(move |_, row| {
        if let Ok(index) = usize::try_from(row.index())
            && let Some((name, target)) = reset_targets.borrow().get(index)
            && let Err(error) = cmd_tx_reset_list.send(CalibrationCommand::Reset {
                target: *target,
                label: name.clone(),
            })
        {
            toasts_for_reset_list.show(&format!("Failed to reset: {error}"));
        }
        reset_popover.popdown();
    });

    let toasts_for_recenter = toasts.clone();
    let cmd_tx_recenter = Rc::clone(&cmd_tx);
    recenter_btn.connect_clicked(move |btn| {
        if let Err(e) = cmd_tx_recenter.send(CalibrationCommand::Recenter) {
            toasts_for_recenter.show(&format!("Failed to start recenter: {e}"));
            return;
        }
        btn.set_label("Recentering...");
        btn.set_sensitive(false);
    });

    let state_for_help = Rc::clone(&state);
    let window_for_help = window.clone();
    help_btn.connect_clicked(move |_| {
        let dialog = build_help_dialog(&state_for_help, false);
        dialog.present(Some(&window_for_help));
    });

    let calibrate_btn_for_msg = calibrate_btn;
    let recenter_btn_for_msg = recenter_btn;
    let toasts_for_msg = toasts;
    let state_for_msg = Rc::clone(&state);
    let source_list_for_msg = Rc::clone(&source_list);
    let target_list_for_msg = Rc::clone(&target_list);
    let background_box_for_msg = background_box;
    let countdown_label_for_msg = countdown_label;
    let progress_fill_for_msg = progress_fill;
    let window_for_msg = window.clone();

    glib::spawn_future_local(async move {
        futures_util::pin_mut!(msg_rx);
        while let Some(msg) = msg_rx.next().await {
            match msg {
                CalibrationMessage::Countdown { seconds } => {
                    if let Some(prev) = toasts_for_msg.current_toast.borrow_mut().take() {
                        prev.dismiss();
                    }
                    calibrate_btn_for_msg.set_label(&format!("{seconds}..."));
                    background_box_for_msg.add_css_class("calibration-bg-active");
                    countdown_label_for_msg.set_text(&format!("{seconds}"));
                    countdown_label_for_msg.set_visible(true);
                    play_sound("message");
                }
                CalibrationMessage::RecenterCountdown { seconds } => {
                    recenter_btn_for_msg.set_label(&format!("{seconds}..."));
                    background_box_for_msg.add_css_class("calibration-bg-active");
                    countdown_label_for_msg.set_text(&format!("{seconds}"));
                    countdown_label_for_msg.set_visible(true);
                    play_sound("message");
                }
                CalibrationMessage::Progress { collected, total } => {
                    calibrate_btn_for_msg.set_label(&format!("{collected}/{total}..."));
                    countdown_label_for_msg.set_visible(false);
                    progress_fill_for_msg.set_visible(true);
                    progress_fill_for_msg.add_css_class("progress-fill-active");
                    let fraction = f64::from(collected) / f64::from(total);
                    update_progress_fill(&progress_fill_for_msg, &window_for_msg, fraction);
                }
                CalibrationMessage::SampledComplete { continuous } => {
                    syncing_continuous.set(true);
                    continuous_btn.set_active(continuous);
                    syncing_continuous.set(false);
                    calibrate_btn_for_msg.set_label("Calibrate");
                    calibrate_btn_for_msg.set_sensitive(true);
                    restore_idle_visuals(
                        &background_box_for_msg,
                        &countdown_label_for_msg,
                        &progress_fill_for_msg,
                    );
                    play_sound("complete");

                    toasts_for_msg.show(if continuous {
                        "Calibration complete; continuous calibration active. Keep devices fixed together."
                    } else {
                        "Calibration complete"
                    });
                }
                CalibrationMessage::ContinuousStopped => {
                    syncing_continuous.set(true);
                    continuous_btn.set_active(false);
                    syncing_continuous.set(false);
                }
                CalibrationMessage::RecenterComplete => {
                    recenter_btn_for_msg.set_label("Recenter");
                    recenter_btn_for_msg.set_sensitive(true);
                    restore_idle_visuals(
                        &background_box_for_msg,
                        &countdown_label_for_msg,
                        &progress_fill_for_msg,
                    );
                    play_sound("complete");
                    toasts_for_msg.show("Centered");
                }
                CalibrationMessage::ResetComplete(label) => {
                    toasts_for_msg.show(&format!("Reset {label}"));
                }
                CalibrationMessage::ProfileRestored { continuous } => {
                    syncing_continuous.set(true);
                    continuous_btn.set_active(continuous);
                    syncing_continuous.set(false);
                    toasts_for_msg.show(if continuous {
                        "Saved calibration restored (continuous mode)"
                    } else {
                        "Saved calibration restored"
                    });
                }
                CalibrationMessage::MovementUpdate { movements } => {
                    state_for_msg
                        .borrow_mut()
                        .set_movement_intensities(movements);
                    update_movement_only(
                        &state_for_msg,
                        &source_list_for_msg,
                        &target_list_for_msg,
                    );
                }
                CalibrationMessage::RuntimeDisconnected | CalibrationMessage::Error(_) => {
                    if matches!(msg, CalibrationMessage::RuntimeDisconnected) {
                        syncing_continuous.set(true);
                        continuous_btn.set_active(false);
                        syncing_continuous.set(false);
                    }
                    calibrate_btn_for_msg.set_label("Calibrate");
                    calibrate_btn_for_msg.set_sensitive(true);
                    recenter_btn_for_msg.set_label("Recenter");
                    recenter_btn_for_msg.set_sensitive(true);
                    restore_idle_visuals(
                        &background_box_for_msg,
                        &countdown_label_for_msg,
                        &progress_fill_for_msg,
                    );
                    if let CalibrationMessage::Error(error) = msg {
                        toasts_for_msg.show(&format!("Error: {error}"));
                    }
                }
            }
        }
    });

    let cmd_tx_for_shutdown = Rc::clone(&cmd_tx);
    let app_for_shutdown = app.clone();
    window.connect_close_request(move |_| {
        let _ = cmd_tx_for_shutdown.send(CalibrationCommand::Shutdown);
        app_for_shutdown.quit();
        glib::Propagation::Proceed
    });

    window.present();
}
