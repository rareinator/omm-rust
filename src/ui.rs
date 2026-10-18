use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::onboard::{self, CopyOutcome, ProfileSector, Snapshot, SwitchOutcome, ToggleOutcome, WriteOutcome};
use crate::profile::{self, DPI_MAX, DPI_MIN, DPI_STEP, Profile, decode_button, dpi_text, dpi_used, hex};

const DEVICE_PAGE: &str = "device";

struct Ui {
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    status: adw::StatusPage,
    status_button: gtk::Button,
    spinner: gtk::Spinner,
    split_view: adw::NavigationSplitView,
    sidebar: gtk::ListBox,
    content: adw::NavigationPage,
    pages: gtk::Stack,
    refresh_button: gtk::Button,
    busy: Cell<bool>,
    /// Validate actions against the device but don't send anything that changes it.
    dry_run: bool,
}

pub fn build(app: &adw::Application, dry_run: bool) {
    let ui = Rc::new(build_widgets(dry_run));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Onboard Memory Manager")
        .default_width(860)
        .default_height(640)
        .width_request(360)
        .height_request(300)
        .content(&ui.toasts)
        .build();

    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 600sp").unwrap());
    breakpoint.add_setter(&ui.split_view, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(breakpoint);

    ui.status_button.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| refresh(&ui)
    ));
    ui.refresh_button.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| refresh(&ui)
    ));
    ui.sidebar.connect_row_activated(glib::clone!(
        #[strong]
        ui,
        move |_, row| {
            ui.select(row);
            ui.split_view.set_show_content(true);
        }
    ));

    window.present();
    refresh(&ui);
}

fn build_widgets(dry_run: bool) -> Ui {
    let spinner = gtk::Spinner::builder().width_request(32).height_request(32).visible(false).build();
    let status_button = gtk::Button::builder()
        .label("Read Device")
        .halign(gtk::Align::Center)
        .css_classes(["suggested-action", "pill"])
        .build();
    let status_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    status_box.append(&spinner);
    status_box.append(&status_button);
    let status = adw::StatusPage::builder().icon_name("input-mouse-symbolic").child(&status_box).build();
    let status_view = adw::ToolbarView::new();
    status_view.add_top_bar(&adw::HeaderBar::new());
    status_view.set_content(Some(&status));

    let refresh_button = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Read the device again")
        .build();
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.pack_start(&refresh_button);
    if dry_run {
        sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Profiles", "Dry run")));
    }
    let sidebar = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
    let sidebar_view = adw::ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_header);
    sidebar_view.set_content(Some(&gtk::ScrolledWindow::builder().child(&sidebar).vexpand(true).build()));
    let sidebar_page = adw::NavigationPage::builder().title("Profiles").child(&sidebar_view).build();

    let pages = gtk::Stack::new();
    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&adw::HeaderBar::new());
    content_view.set_content(Some(&pages));
    let content = adw::NavigationPage::builder().title("Device").child(&content_view).build();

    let split_view = adw::NavigationSplitView::builder().sidebar(&sidebar_page).content(&content).build();

    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
    stack.add_named(&status_view, Some("status"));
    stack.add_named(&split_view, Some("split"));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));

    Ui {
        toasts,
        stack,
        status,
        status_button,
        spinner,
        split_view,
        sidebar,
        content,
        pages,
        refresh_button,
        busy: Cell::new(false),
        dry_run,
    }
}

/// Read the device on a worker thread; only one read runs at a time.
fn refresh(ui: &Rc<Ui>) {
    if ui.busy.replace(true) {
        return;
    }
    ui.show_loading();
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        match gio::spawn_blocking(onboard::read_snapshot).await {
            Ok(Ok(snapshot)) => ui.show_snapshot(&snapshot),
            Ok(Err(msg)) => ui.show_error(&msg),
            Err(_) => ui.show_error("The device reader crashed"),
        }
        ui.busy.set(false);
        ui.refresh_button.set_sensitive(true);
    });
}

/// Run a device operation on a worker thread, one at a time; errors become toasts.
fn run_op<T: Send + 'static>(
    ui: &Rc<Ui>,
    op: impl FnOnce() -> Result<T, String> + Send + 'static,
    done: impl FnOnce(&Rc<Ui>, T) + 'static,
) {
    if ui.busy.replace(true) {
        return;
    }
    ui.refresh_button.set_sensitive(false);
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(op).await;
        ui.busy.set(false);
        ui.refresh_button.set_sensitive(true);
        match result {
            Ok(Ok(value)) => done(&ui, value),
            Ok(Err(msg)) => ui.toast(&msg),
            Err(_) => ui.toast("The device worker crashed"),
        }
    });
}

/// Send SetActiveProfile, then re-read the device to show the result.
fn switch_profile(ui: &Rc<Ui>, sector: u16) {
    let dry_run = ui.dry_run;
    run_op(ui, move || onboard::set_active_profile(sector, dry_run), |ui, outcome| match outcome {
        SwitchOutcome::Switched { from, to } => {
            ui.toast(&format!("Switched from profile {from} to profile {to}"));
            refresh(ui);
        }
        SwitchOutcome::DryRun { to, report } => {
            ui.toast(&format!("Dry run: would send {} to activate profile {to}", hex(&report)));
        }
    });
}

/// Write the DPI table of a profile, then re-read the device to show the result.
fn write_dpi(ui: &Rc<Ui>, sector: u16, dpi: [u16; 5]) {
    let dry_run = ui.dry_run;
    run_op(ui, move || onboard::write_profile_dpi(sector, dpi, dry_run), |ui, outcome| match outcome {
        WriteOutcome::NoChange => ui.toast("The device already has these DPI values"),
        WriteOutcome::Written { sector, changes, backup, reactivated } => {
            let reload = if reactivated { ", active profile reloaded" } else { "" };
            ui.toast(&format!(
                "Profile {sector}: {} written and verified{reload}. Backup: {}",
                summarize(&changes),
                backup.file_name().unwrap_or_default().to_string_lossy()
            ));
            refresh(ui);
        }
        WriteOutcome::DryRun { sector, changes, new_crc, requests } => {
            ui.toast(&format!(
                "Dry run: profile {sector} {}, new CRC {new_crc:#06x}; {requests} write requests not sent",
                summarize(&changes)
            ));
        }
    });
}

/// Copy one profile over another (optionally enabling it), then re-read the device.
fn copy_profile(ui: &Rc<Ui>, source: u16, target: u16, enable: bool) {
    let dry_run = ui.dry_run;
    run_op(ui, move || onboard::copy_profile(source, target, enable, dry_run), |ui, outcome| match outcome {
        CopyOutcome::NoChange => ui.toast("Nothing to do: the profiles are already identical"),
        CopyOutcome::Copied { source, target, backups, enabled, reactivated } => {
            let mut done = vec![format!("Copied profile {source} into profile {target}")];
            if enabled {
                done.push(format!("enabled profile {target}"));
            }
            if reactivated {
                done.push("reloaded it".into());
            }
            let names: Vec<String> =
                backups.iter().map(|b| b.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
            ui.toast(&format!("{}. Backups: {}", done.join(", "), names.join(", ")));
            refresh(ui);
        }
        CopyOutcome::DryRun { source, target, copy, enable, requests } => {
            let mut plan = Vec::new();
            if copy {
                plan.push(format!("copy profile {source} into profile {target}"));
            }
            if enable {
                plan.push(format!("enable profile {target}"));
            }
            ui.toast(&format!("Dry run: would {}; {requests} write requests not sent", plan.join(" and ")));
        }
    });
}

/// Enable or disable a profile in the directory, then re-read the device.
fn set_enabled(ui: &Rc<Ui>, sector: u16, enabled: bool) {
    let dry_run = ui.dry_run;
    run_op(ui, move || onboard::set_profile_enabled(sector, enabled, dry_run), |ui, outcome| match outcome {
        ToggleOutcome::NoChange => ui.toast("The directory already has this state"),
        ToggleOutcome::Written { sector, enabled, backup } => {
            let verb = if enabled { "Enabled" } else { "Disabled" };
            ui.toast(&format!(
                "{verb} profile {sector}. Backup: {}",
                backup.file_name().unwrap_or_default().to_string_lossy()
            ));
            refresh(ui);
        }
        ToggleOutcome::DryRun { sector, enabled, entry, requests } => {
            let verb = if enabled { "enable" } else { "disable" };
            ui.toast(&format!(
                "Dry run: would {verb} profile {sector} (flags {:#04x}, extra {:#04x}); {requests} write requests not sent",
                entry.flags, entry.extra
            ));
        }
    });
}

/// "Enabled" row with an Enable/Disable button that asks for confirmation first.
fn enabled_row(ui: &Rc<Ui>, s: &Snapshot, p: &ProfileSector) -> adw::ActionRow {
    let sector = p.entry.sector;
    let enabled = p.entry.enabled();
    let blocker = if enabled && sector == s.active_sector {
        Some("This is the active profile; make another profile active first")
    } else if !enabled && !p.crc_ok {
        Some("This profile has no valid data; copy a profile into it first")
    } else {
        None
    };
    let button = gtk::Button::builder()
        .label(if enabled { "Disable" } else { "Enable" })
        .valign(gtk::Align::Center)
        .sensitive(blocker.is_none())
        .build();
    let state = if enabled { "Yes" } else { "No" };
    let subtitle = match blocker {
        Some(why) => format!("{state}. {why}"),
        None => format!("{state}. Disabled profiles are skipped when cycling profiles"),
    };
    let row = adw::ActionRow::builder().title("Enabled").subtitle(subtitle).use_markup(false).build();
    row.add_suffix(&button);
    button.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |button| confirm_enabled(&ui, button.upcast_ref(), sector, !enabled)
    ));
    row
}

fn confirm_enabled(ui: &Rc<Ui>, parent: &gtk::Widget, sector: u16, enabled: bool) {
    let (heading, body) = if enabled {
        (format!("Enable Profile {sector}?"), "The mouse can then switch to this profile.")
    } else {
        (format!("Disable Profile {sector}?"), "Its settings stay stored, and it can be enabled again later.")
    };
    let note = if ui.dry_run {
        "Dry run: nothing will be written."
    } else {
        "This rewrites the profile directory in the mouse's flash memory. A backup is saved first."
    };
    let dialog = adw::AlertDialog::new(Some(&heading), Some(&format!("{body}\n\n{note}")));
    let label = if enabled { "Enable" } else { "Disable" };
    dialog.add_responses(&[("cancel", "Cancel"), ("apply", label)]);
    dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(Some("apply"), glib::clone!(
        #[strong]
        ui,
        move |_, _| set_enabled(&ui, sector, enabled)
    ));
    dialog.present(Some(parent));
}

fn profile_label(s: &Snapshot, p: &ProfileSector) -> String {
    let sector = p.entry.sector;
    let name = p.profile.as_ref().and_then(|p| p.name.clone()).unwrap_or_else(|| format!("Profile {sector}"));
    let state = if sector == s.active_sector { "active" } else if p.entry.enabled() { "enabled" } else { "disabled" };
    let dpi: Vec<String> = p
        .profile
        .iter()
        .flat_map(|p| p.dpi)
        .filter(|&v| dpi_used(v))
        .map(|v| v.to_string())
        .collect();
    format!("{name} ({state}, DPI {})", dpi.join("/"))
}

/// "Copy From…" button: overwrite this profile with another valid one.
fn copy_button(ui: &Rc<Ui>, s: &Snapshot, p: &ProfileSector) -> gtk::Button {
    let target = p.entry.sector;
    let target_enabled = p.entry.enabled();
    let sources: Vec<(u16, String)> = s
        .profiles
        .iter()
        .filter(|o| o.entry.sector != target && o.crc_ok)
        .map(|o| (o.entry.sector, profile_label(s, o)))
        .collect();
    let button = gtk::Button::builder()
        .label("Copy From…")
        .valign(gtk::Align::Center)
        .sensitive(!sources.is_empty())
        .tooltip_text(if sources.is_empty() {
            "No other profile has valid data"
        } else {
            "Replace this profile with a copy of another one"
        })
        .build();
    button.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |button| confirm_copy(&ui, button.upcast_ref(), &sources, target, target_enabled)
    ));
    button
}

fn confirm_copy(ui: &Rc<Ui>, parent: &gtk::Widget, sources: &[(u16, String)], target: u16, target_enabled: bool) {
    let labels: Vec<&str> = sources.iter().map(|(_, l)| l.as_str()).collect();
    let source_pick = gtk::DropDown::from_strings(&labels);
    let enable = gtk::CheckButton::builder()
        .label(format!("Also enable profile {target}"))
        .active(true)
        .visible(!target_enabled)
        .build();
    let extra = gtk::Box::new(gtk::Orientation::Vertical, 12);
    extra.append(&source_pick);
    extra.append(&enable);

    let note = if ui.dry_run {
        "Dry run: nothing will be written."
    } else {
        "This writes to the mouse's flash memory. Backups of every changed sector are saved first."
    };
    let dialog = adw::AlertDialog::new(
        Some(&format!("Copy Into Profile {target}?")),
        Some(&format!(
            "Everything in profile {target} (DPI, buttons and other settings) is replaced by the profile chosen below.\n\n{note}"
        )),
    );
    dialog.set_extra_child(Some(&extra));
    dialog.add_responses(&[("cancel", "Cancel"), ("copy", "Copy")]);
    dialog.set_response_appearance("copy", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let sectors: Vec<u16> = sources.iter().map(|(n, _)| *n).collect();
    dialog.connect_response(Some("copy"), glib::clone!(
        #[strong]
        ui,
        move |_, _| {
            let Some(&source) = sectors.get(source_pick.selected() as usize) else {
                return;
            };
            copy_profile(&ui, source, target, !target_enabled && enable.is_active());
        }
    ));
    dialog.present(Some(parent));
}

fn summarize(changes: &[onboard::DpiChange]) -> String {
    changes.iter().map(|c| format!("slot {} {} → {}", c.slot, dpi_text(c.old), dpi_text(c.new))).collect::<Vec<_>>().join(", ")
}

fn confirm_dpi_write(ui: &Rc<Ui>, parent: &gtk::Widget, sector: u16, original: [u16; 5], dpi: [u16; 5]) {
    let changes: Vec<String> = (0..5)
        .filter(|&i| original[i] != dpi[i])
        .map(|i| format!("Slot {i}: {} → {}", dpi_text(original[i]), dpi_text(dpi[i])))
        .collect();
    let note = if ui.dry_run {
        "Dry run: nothing will be written."
    } else {
        "This writes the profile to the mouse's flash memory. A backup of the current sector is saved first."
    };
    let dialog = adw::AlertDialog::new(
        Some(&format!("Write DPI to Profile {sector}?")),
        Some(&format!("{}\n\n{note}", changes.join("\n"))),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("write", "Write")]);
    dialog.set_response_appearance("write", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(Some("write"), glib::clone!(
        #[strong]
        ui,
        move |_, _| write_dpi(&ui, sector, dpi)
    ));
    dialog.present(Some(parent));
}

impl Ui {
    fn toast(&self, msg: &str) {
        let toast = adw::Toast::builder().title(glib::markup_escape_text(msg)).timeout(5).build();
        self.toasts.add_toast(toast);
    }

    fn show_loading(&self) {
        self.refresh_button.set_sensitive(false);
        self.status.set_title("Reading Device…");
        self.status.set_description(None);
        self.status_button.set_visible(false);
        self.spinner.set_visible(true);
        self.spinner.start();
        self.stack.set_visible_child_name("status");
    }

    fn show_error(&self, msg: &str) {
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.status.set_title("Could Not Read Device");
        self.status.set_description(Some(&glib::markup_escape_text(msg)));
        self.status_button.set_label("Retry");
        self.status_button.set_visible(true);
        self.stack.set_visible_child_name("status");
    }

    fn show_snapshot(self: &Rc<Self>, s: &Snapshot) {
        self.spinner.stop();
        self.sidebar.remove_all();
        while let Some(child) = self.pages.first_child() {
            self.pages.remove(&child);
        }

        self.add_item(DEVICE_PAGE, "Device", &s.node.name, &device_page(s));
        let mut active_row = None;
        for p in &s.profiles {
            let sector = p.entry.sector;
            let is_active = sector == s.active_sector;
            let mut tags = vec![if p.entry.enabled() { "Enabled" } else { "Disabled" }];
            if is_active {
                tags.insert(0, "Active");
            }
            if !p.crc_ok {
                tags.push("Bad CRC");
            }
            let name = p.profile.as_ref().and_then(|p| p.name.clone());
            let title = name.unwrap_or_else(|| format!("Profile {sector}"));
            let row = self.add_item(&format!("profile-{sector}"), &title, &tags.join(" · "),
                                    &profile_page(self, s, p));
            if is_active {
                active_row = Some(row);
            }
        }

        let row = active_row.or_else(|| self.sidebar.row_at_index(0));
        if let Some(row) = row {
            self.sidebar.select_row(Some(&row));
            self.select(&row);
        }
        self.stack.set_visible_child_name("split");
    }

    fn add_item(&self, name: &str, title: &str, subtitle: &str, page: &impl IsA<gtk::Widget>) -> gtk::ListBoxRow {
        self.pages.add_titled(page, Some(name), title);
        let title_label = gtk::Label::builder().label(title).xalign(0.0).build();
        let subtitle_label = gtk::Label::builder()
            .label(subtitle)
            .xalign(0.0)
            .css_classes(["caption", "dim-label"])
            .build();
        let vbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        vbox.append(&title_label);
        vbox.append(&subtitle_label);
        let row = gtk::ListBoxRow::builder().child(&vbox).name(name).build();
        self.sidebar.append(&row);
        row
    }

    fn select(&self, row: &gtk::ListBoxRow) {
        let name = row.widget_name();
        self.pages.set_visible_child_name(&name);
        if let Some(page) = self.pages.child_by_name(&name) {
            let title = self.pages.page(&page).title().unwrap_or_default();
            self.content.set_title(&title);
        }
    }
}

fn info_row(title: &str, value: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .use_markup(false)
        .subtitle_selectable(true)
        .css_classes(["property"])
        .build()
}

fn group(title: &str) -> adw::PreferencesGroup {
    adw::PreferencesGroup::builder().title(title).build()
}

fn ok_text(ok: bool) -> &'static str {
    if ok { "OK" } else { "Mismatch" }
}

fn device_page(s: &Snapshot) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();

    let dev = group("Device");
    dev.add(&info_row("Name", &s.node.name));
    dev.add(&info_row("Interface", &s.node.path.display().to_string()));
    dev.add(&info_row("Device index", &format!("{:#04x}", s.device_index)));
    dev.add(&info_row("HID++ version", &format!("{}.{}", s.protocol.0, s.protocol.1)));
    dev.add(&info_row("Onboard mode", &format!("{} ({})", s.mode_name(), s.mode)));
    dev.add(&info_row("Active profile", &format!("Sector {:#06x}", s.active_sector)));
    page.add(&dev);

    let i = &s.info;
    let mem = group("Onboard Memory");
    let fmt_note = if profile::is_supported_format(i.profile_format) { "" } else { " (layout not decoded)" };
    mem.add(&info_row("Feature 0x8100 index", &format!("{:#04x}", s.feature_index)));
    mem.add(&info_row("Profile format", &format!("{}{fmt_note}", i.profile_format)));
    mem.add(&info_row("Macro format", &i.macro_format.to_string()));
    mem.add(&info_row("Profiles", &format!("{} (+{} ROM)", i.profile_count, i.rom_profile_count)));
    mem.add(&info_row("Buttons", &i.button_count.to_string()));
    mem.add(&info_row("Sectors", &format!("{} × {} bytes", i.sector_count, i.sector_size)));
    mem.add(&info_row("Memory model", &i.memory_model.to_string()));
    mem.add(&info_row("Directory CRC", ok_text(s.directory_crc_ok)));
    mem.add(&info_row("GetInfo reply", &hex(&i.raw)));
    page.add(&mem);
    page
}

/// Header button for the "Profile" group; returns None for the active profile.
fn activate_button(ui: &Rc<Ui>, s: &Snapshot, p: &ProfileSector) -> Option<gtk::Button> {
    let sector = p.entry.sector;
    if sector == s.active_sector {
        return None;
    }
    let blocker = if !p.entry.enabled() {
        Some("This profile is disabled in the directory")
    } else if !p.crc_ok {
        Some("This profile has no valid data")
    } else {
        None
    };
    let button = gtk::Button::builder()
        .label("Make Active")
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .sensitive(blocker.is_none())
        .tooltip_text(blocker.unwrap_or("Switch the mouse to this profile (no flash write)"))
        .build();
    button.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |_| switch_profile(&ui, sector)
    ));
    Some(button)
}

struct SlotRow {
    switch: gtk::Switch,
    adj: gtk::Adjustment,
    scale: gtk::Scale,
    spin: gtk::SpinButton,
    row: adw::ActionRow,
    tags: String,
}

/// Initial slider position for a slot: its own value, else the nearest used slot before it.
fn slider_start(dpi: &[u16; 5], slot: usize) -> u16 {
    (0..=slot).rev().map(|i| dpi[i]).find(|&v| dpi_used(v)).unwrap_or(800).clamp(DPI_MIN, DPI_MAX)
}

/// DPI slots with an on/off switch and a slider each. Edits stay local until Apply.
fn dpi_group(ui: &Rc<Ui>, sector: u16, prof: &Profile) -> adw::PreferencesGroup {
    let g = group("DPI");
    let original = prof.dpi;
    let (default_slot, shift_slot) = (prof.default_dpi_slot, prof.shift_dpi_slot);
    let edited = Rc::new(Cell::new(original));

    let revert = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .tooltip_text("Revert to the values on the device")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    let apply = gtk::Button::builder()
        .label("Apply")
        .tooltip_text("Write the DPI table to the mouse")
        .valign(gtk::Align::Center)
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.append(&revert);
    header.append(&apply);
    g.set_header_suffix(Some(&header));

    let rows: Rc<Vec<SlotRow>> = Rc::new(
        (0..5)
            .map(|slot| {
                let mut tags = Vec::new();
                if slot == default_slot as usize {
                    tags.push("Default");
                }
                if slot == shift_slot as usize {
                    tags.push("Shift");
                }
                let start = slider_start(&original, slot) as f64;
                let adj = gtk::Adjustment::new(start, DPI_MIN as f64, DPI_MAX as f64, DPI_STEP as f64, 400.0, 0.0);
                let scale = gtk::Scale::builder()
                    .adjustment(&adj)
                    .hexpand(true)
                    .width_request(140)
                    .valign(gtk::Align::Center)
                    .build();
                let spin = gtk::SpinButton::builder()
                    .adjustment(&adj)
                    .numeric(true)
                    .width_chars(5)
                    .valign(gtk::Align::Center)
                    .build();
                let switch = gtk::Switch::builder().active(dpi_used(original[slot])).valign(gtk::Align::Center).build();
                let row = adw::ActionRow::builder().title(format!("Slot {slot}")).use_markup(false).build();
                row.add_prefix(&switch);
                row.add_suffix(&scale);
                row.add_suffix(&spin);
                g.add(&row);
                SlotRow { switch, adj, scale, spin, row, tags: tags.join(", ") }
            })
            .collect(),
    );

    // Recompute which switches may flip, the rows' state and the Apply/Revert buttons.
    let sync = Rc::new(glib::clone!(
        #[strong]
        edited,
        #[weak]
        apply,
        #[weak]
        revert,
        #[weak]
        rows,
        move || {
            let dpi = edited.get();
            for (slot, r) in rows.iter().enumerate() {
                let on = dpi_used(dpi[slot]);
                let locked = profile::dpi_slot_locked(slot, default_slot, shift_slot);
                let (can_flip, why) = if on {
                    if locked {
                        (false, "The default and shift slots can't be turned off")
                    } else if slot + 1 < 5 && dpi_used(dpi[slot + 1]) {
                        (false, "Turn off the later slots first; used slots must be contiguous")
                    } else {
                        (true, "Mark this slot unused")
                    }
                } else if slot > 0 && !dpi_used(dpi[slot - 1]) {
                    (false, "Turn on the earlier slots first; used slots must be contiguous")
                } else {
                    (true, "Use this slot")
                };
                r.switch.set_sensitive(can_flip);
                r.switch.set_tooltip_text(Some(why));
                r.scale.set_sensitive(on);
                r.spin.set_sensitive(on);
                let subtitle = match (on, r.tags.is_empty()) {
                    (true, _) => r.tags.clone(),
                    (false, true) => "Unused".into(),
                    (false, false) => format!("Unused ({})", r.tags),
                };
                r.row.set_subtitle(&subtitle);
            }
            let changed = dpi != original;
            let valid = profile::validate_dpi_table(&dpi, default_slot, shift_slot).is_ok();
            apply.set_sensitive(changed && valid);
            revert.set_sensitive(changed);
        }
    ));

    for (slot, r) in rows.iter().enumerate() {
        r.adj.connect_value_changed(glib::clone!(
            #[strong]
            edited,
            #[strong]
            sync,
            #[weak(rename_to = switch)]
            r.switch,
            move |adj| {
                let step = DPI_STEP as f64;
                let snapped = (adj.value() / step).round() * step;
                if adj.value() != snapped {
                    adj.set_value(snapped); // re-enters with the snapped value
                    return;
                }
                if switch.is_active() {
                    let mut dpi = edited.get();
                    dpi[slot] = snapped as u16;
                    edited.set(dpi);
                }
                sync();
            }
        ));
        r.switch.connect_active_notify(glib::clone!(
            #[strong]
            edited,
            #[strong]
            sync,
            #[weak(rename_to = adj)]
            r.adj,
            move |switch| {
                let mut dpi = edited.get();
                dpi[slot] = match (switch.is_active(), dpi_used(original[slot])) {
                    (true, _) => adj.value() as u16,
                    (false, false) => original[slot], // keep the device's own unused marker
                    (false, true) => profile::DPI_UNUSED,
                };
                edited.set(dpi);
                sync();
            }
        ));
    }
    sync();

    revert.connect_clicked(glib::clone!(
        #[strong]
        rows,
        move |_| {
            // Slider first: turning a slot on copies the slider into the edited table
            for (slot, r) in rows.iter().enumerate() {
                r.adj.set_value(slider_start(&original, slot) as f64);
                r.switch.set_active(dpi_used(original[slot]));
            }
        }
    ));
    apply.connect_clicked(glib::clone!(
        #[strong]
        ui,
        move |button| confirm_dpi_write(&ui, button.upcast_ref(), sector, original, edited.get())
    ));
    g
}

fn profile_page(ui: &Rc<Ui>, s: &Snapshot, p: &ProfileSector) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    let sector = p.entry.sector;
    let stored = profile::stored_crc(&p.raw);
    let computed = profile::crc16_ccitt(&p.raw[..p.raw.len() - 2]);

    let overview = group("Profile");
    let state = match (sector == s.active_sector, p.entry.enabled()) {
        (true, _) => "Active",
        (false, true) => "Enabled",
        (false, false) => "Disabled",
    };
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.append(&copy_button(ui, s, p));
    if let Some(button) = activate_button(ui, s, p) {
        actions.append(&button);
    }
    overview.set_header_suffix(Some(&actions));
    overview.add(&info_row("State", state));
    overview.add(&enabled_row(ui, s, p));
    overview.add(&info_row("Sector", &format!("{sector:#06x}")));
    overview.add(&info_row(
        "Directory entry",
        &format!("flags {:#04x}, extra {:#04x}", p.entry.flags, p.entry.extra),
    ));
    overview.add(&info_row(
        "CRC",
        &format!("{} (stored {stored:#06x}, computed {computed:#06x})", ok_text(p.crc_ok)),
    ));
    page.add(&overview);

    match &p.profile {
        Some(prof) => {
            overview.add(&info_row("Name", prof.name.as_deref().unwrap_or("Unnamed")));
            let rate = match prof.report_rate_hz() {
                Some(hz) => format!("~{hz} Hz (raw {:#04x})", prof.report_rate),
                None => format!("raw {:#04x}", prof.report_rate),
            };
            overview.add(&info_row("Report rate", &rate));

            page.add(&dpi_group(ui, sector, prof));

            for (title, table) in [("Buttons", &prof.buttons), ("G-Shift Buttons", &prof.gshift_buttons)] {
                let g = group(title);
                for (slot, entry) in table.iter().enumerate() {
                    let row = info_row(&format!("{slot}: {}", profile::slot_name(slot)), &decode_button(entry));
                    row.add_suffix(&gtk::Label::builder().label(hex(entry)).css_classes(["monospace", "dim-label"]).build());
                    g.add(&row);
                }
                page.add(&g);
            }

            let other = group("Other");
            other.add(&info_row("LED colour (RGB)", &hex(&prof.led_rgb)));
            other.add(&info_row("Power mode", &format!("{:#04x}", prof.power_mode)));
            other.add(&info_row("Angle snapping", &format!("{:#04x}", prof.angle_snapping)));
            page.add(&other);
        }
        None => {
            let reason = if !p.crc_ok {
                "The CRC doesn't match, so this sector is empty or was never written."
            } else {
                "This device's profile format uses a layout that isn't decoded yet."
            };
            overview.set_description(Some(reason));
        }
    }

    let raw = group("Raw Sector");
    let expander = adw::ExpanderRow::builder().title("Hex dump").subtitle(format!("{} bytes", p.raw.len())).build();
    expander.add_row(
        &gtk::Label::builder()
            .label(hexdump(&p.raw))
            .xalign(0.0)
            .selectable(true)
            .css_classes(["monospace"])
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build(),
    );
    raw.add(&expander);
    page.add(&raw);
    page
}

fn hexdump(data: &[u8]) -> String {
    data.chunks(16)
        .enumerate()
        .map(|(i, row)| format!("{:04x}: {}", i * 16, hex(row)))
        .collect::<Vec<_>>()
        .join("\n")
}
