use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::gdk;
use gtk4::{
    Box as GtkBox, Button, CssProvider, Label, ListBox, ListBoxRow, Orientation,
    Popover, ScrolledWindow, SelectionMode, PolicyType,
};

use crate::ui::device_selector::Device;

type SelectionCallback = Box<dyn Fn(Option<Device>)>;
type PopoverCallback = Box<dyn Fn(PopoverState)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopoverState {
    Opened,
    Closed,
}

struct HighlightableRow {
    row: ListBoxRow,
    css_provider: CssProvider,
}

pub struct DeviceList {
    container: GtkBox,
    button: Button,
    button_label: Label,
    popover: Popover,
    scrolled: ScrolledWindow,
    list_box: ListBox,
    devices: RefCell<Vec<Device>>,
    selected_id: RefCell<Option<String>>,
    movement_intensities: RefCell<HashMap<String, f32>>,
    device_rows: RefCell<HashMap<String, HighlightableRow>>,
    on_changed: RefCell<Option<SelectionCallback>>,
    on_popover_visibility: RefCell<Option<PopoverCallback>>,
}

impl DeviceList {
    pub fn new(label_text: &str) -> Rc<Self> {
        let container = GtkBox::new(Orientation::Vertical, 8);

        let label = Label::new(Some(label_text));
        label.set_xalign(0.5);
        label.add_css_class("heading");
        container.append(&label);

        let button = Button::new();
        button.add_css_class("outlined");

        let button_label = Label::new(Some("(None)"));
        button_label.set_xalign(0.5);
        button_label.set_margin_start(12);
        button_label.set_margin_end(12);
        button_label.set_margin_top(8);
        button_label.set_margin_bottom(8);
        button_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        button_label.set_max_width_chars(24);
        button.set_child(Some(&button_label));

        container.append(&button);

        let popover = Popover::new();
        popover.set_parent(&button);
        popover.set_has_arrow(false);
        popover.set_autohide(true);

        let scrolled = ScrolledWindow::new();
        scrolled.set_policy(PolicyType::Never, PolicyType::Automatic);
        scrolled.set_propagate_natural_height(true);

        let list_box = ListBox::new();
        list_box.set_selection_mode(SelectionMode::None);
        list_box.add_css_class("boxed-list");
        list_box.set_margin_start(6);
        list_box.set_margin_end(6);
        list_box.set_margin_top(6);
        list_box.set_margin_bottom(6);

        scrolled.set_child(Some(&list_box));
        popover.set_child(Some(&scrolled));

        let this = Rc::new(Self {
            container,
            button,
            button_label,
            popover,
            scrolled,
            list_box,
            devices: RefCell::new(Vec::new()),
            selected_id: RefCell::new(None),
            movement_intensities: RefCell::new(HashMap::new()),
            device_rows: RefCell::new(HashMap::new()),
            on_changed: RefCell::new(None),
            on_popover_visibility: RefCell::new(None),
        });

        let this_for_click = Rc::clone(&this);
        this.button.connect_clicked(move |btn| {
            // Match popover width to button width
            let button_width = btn.width();
            this_for_click.scrolled.set_min_content_width(button_width.max(200));

            this_for_click.rebuild_list();
            this_for_click.popover.popup();
        });

        let this_for_show = Rc::clone(&this);
        this.popover.connect_show(move |_| {
            if let Some(ref cb) = *this_for_show.on_popover_visibility.borrow() {
                cb(PopoverState::Opened);
            }
        });

        let this_for_closed = Rc::clone(&this);
        this.popover.connect_closed(move |_| {
            if let Some(ref cb) = *this_for_closed.on_popover_visibility.borrow() {
                cb(PopoverState::Closed);
            }
        });

        let this_for_activate = Rc::clone(&this);
        this.list_box.connect_row_activated(move |_, row| {
            this_for_activate.handle_row_activation(row);
        });

        this
    }

    pub const fn widget(&self) -> &GtkBox {
        &self.container
    }

    fn rebuild_list(self: &Rc<Self>) {
        let display = gdk::Display::default().expect("No display");
        for highlightable in self.device_rows.borrow().values() {
            gtk4::style_context_remove_provider_for_display(
                &display,
                &highlightable.css_provider,
            );
        }

        while let Some(row) = self.list_box.first_child() {
            self.list_box.remove(&row);
        }
        self.device_rows.borrow_mut().clear();

        let devices = self.devices.borrow();
        let intensities = self.movement_intensities.borrow();
        let selected = self.selected_id.borrow().clone();

        // Add "(None)" option
        let none_row = Self::create_device_row(None, selected.is_none(), 0.0);
        self.list_box.append(&none_row.row);

        let mut current_category = String::new();

        for device in devices.iter() {
            if device.category != current_category {
                current_category.clone_from(&device.category);
                let cat_row = ListBoxRow::new();
                cat_row.set_selectable(false);
                cat_row.set_activatable(false);

                let cat_label = Label::new(Some(&current_category));
                cat_label.set_xalign(0.0);
                cat_label.add_css_class("dim-label");
                cat_label.add_css_class("caption-heading");
                cat_label.set_margin_start(8);
                cat_label.set_margin_top(12);
                cat_label.set_margin_bottom(4);
                cat_row.set_child(Some(&cat_label));
                self.list_box.append(&cat_row);
            }

            let device_id = device.unique_id();
            let is_selected = selected.as_deref() == Some(device_id);
            let intensity = intensities.get(device_id).copied().unwrap_or(0.0);
            let highlightable = Self::create_device_row(Some(device), is_selected, intensity);
            self.list_box.append(&highlightable.row);

            // Store reference for live updates (keyed by unique_id)
            self.device_rows.borrow_mut().insert(device_id.to_string(), highlightable);
        }
    }

    fn handle_row_activation(self: &Rc<Self>, row: &ListBoxRow) {
        let index = row.index();

        if index == 0 {
            // "(None)" selected
            *self.selected_id.borrow_mut() = None;
            self.button_label.set_text("(None)");
            self.button_label.remove_css_class("accent");
            if let Some(ref cb) = *self.on_changed.borrow() {
                cb(None);
            }
        } else {
            let devices = self.devices.borrow();
            let mut current_idx = 1; // Start after "(None)"
            let mut current_cat = String::new();

            for device in devices.iter() {
                if device.category != current_cat {
                    current_cat.clone_from(&device.category);
                    current_idx += 1; // Skip category header
                }

                if current_idx == index {
                    // Store unique_id for matching
                    *self.selected_id.borrow_mut() = Some(device.unique_id().to_string());
                    self.button_label.set_text(&device.display_name());
                    self.button_label.add_css_class("accent");
                    let device = device.clone();
                    drop(devices);
                    if let Some(ref cb) = *self.on_changed.borrow() {
                        cb(Some(device));
                    }
                    break;
                }
                current_idx += 1;
            }
        }

        self.popover.popdown();
    }

    fn create_device_row(device: Option<&Device>, is_selected: bool, intensity: f32) -> HighlightableRow {
        static ROW_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let row_id = ROW_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let row = ListBoxRow::new();
        row.set_selectable(false);

        // Unique CSS class for this row
        let css_class = format!("device-row-{row_id}");
        row.add_css_class(&css_class);

        // Per-row CSS provider for smooth intensity updates
        let css_provider = CssProvider::new();
        gtk4::style_context_add_provider_for_display(
            &gdk::Display::default().expect("No display"),
            &css_provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );

        let hbox = GtkBox::new(Orientation::Horizontal, 12);
        hbox.set_margin_start(12);
        hbox.set_margin_end(12);
        hbox.set_margin_top(10);
        hbox.set_margin_bottom(10);

        let name = device.map_or_else(|| "(None)".to_string(), super::device_selector::Device::display_name);
        let name_label = Label::new(Some(&name));
        name_label.set_xalign(0.0);
        name_label.set_hexpand(true);
        if is_selected {
            name_label.add_css_class("accent");
        }
        hbox.append(&name_label);

        if is_selected {
            let check = Label::new(Some("✓"));
            check.add_css_class("accent");
            hbox.append(&check);
        }

        row.set_child(Some(&hbox));

        Self::apply_intensity(&css_provider, &css_class, intensity);

        HighlightableRow { row, css_provider }
    }

    fn apply_intensity(css_provider: &CssProvider, css_class: &str, intensity: f32) {
        let alpha = intensity * 0.5;
        let css = format!(
            ".{css_class} {{ background-color: rgba(74, 222, 128, {alpha:.3}); }}"
        );
        css_provider.load_from_string(&css);
    }

    pub fn set_devices(&self, devices: Vec<Device>, selected_id: Option<&str>) {
        *self.selected_id.borrow_mut() = selected_id.map(String::from);

        if let Some(id) = selected_id {
            if let Some(device) = devices.iter().find(|d| d.unique_id() == id) {
                self.button_label.set_text(&device.display_name());
                self.button_label.add_css_class("accent");
            } else {
                self.button_label.set_text(id);
                self.button_label.add_css_class("accent");
            }
        } else {
            self.button_label.set_text("(None)");
            self.button_label.remove_css_class("accent");
        }

        *self.devices.borrow_mut() = devices;
    }

    pub fn update_movement(&self, intensities: HashMap<String, f32>) {
        if self.popover.is_visible() {
            let device_rows = self.device_rows.borrow();
            for (name, highlightable) in device_rows.iter() {
                let intensity = intensities.get(name).copied().unwrap_or(0.0);
                if let Some(css_class) = highlightable.row.css_classes().iter()
                    .find(|c| c.starts_with("device-row-"))
                    .map(std::string::ToString::to_string)
                {
                    Self::apply_intensity(&highlightable.css_provider, &css_class, intensity);
                }
            }
            drop(device_rows);
        }
        *self.movement_intensities.borrow_mut() = intensities;
    }

    pub fn connect_changed<F: Fn(Option<Device>) + 'static>(self: &Rc<Self>, callback: F) {
        *self.on_changed.borrow_mut() = Some(Box::new(callback));
    }

    pub fn connect_popover_visibility<F: Fn(PopoverState) + 'static>(self: &Rc<Self>, callback: F) {
        *self.on_popover_visibility.borrow_mut() = Some(Box::new(callback));
    }
}
