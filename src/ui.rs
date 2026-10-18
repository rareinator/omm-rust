use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::onboard::{self, ProfileSector, Snapshot};
use crate::profile::{self, decode_button, dpi_used, hex};

const DEVICE_PAGE: &str = "device";

struct Ui {
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
}

pub fn build(app: &adw::Application) {
    let ui = Rc::new(build_widgets());

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Onboard Memory Manager")
        .default_width(860)
        .default_height(640)
        .width_request(360)
        .height_request(300)
        .content(&ui.stack)
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

fn build_widgets() -> Ui {
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
    let sidebar = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
    let sidebar_view = adw::ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_header);
    sidebar_view.set_content(Some(&gtk::ScrolledWindow::builder().child(&sidebar).vexpand(true).build()));
    let sidebar_page = adw::NavigationPage::builder().title("Onboard Memory").child(&sidebar_view).build();

    let pages = gtk::Stack::new();
    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&adw::HeaderBar::new());
    content_view.set_content(Some(&pages));
    let content = adw::NavigationPage::builder().title("Device").child(&content_view).build();

    let split_view = adw::NavigationSplitView::builder().sidebar(&sidebar_page).content(&content).build();

    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
    stack.add_named(&status_view, Some("status"));
    stack.add_named(&split_view, Some("split"));

    Ui {
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

impl Ui {
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

    fn show_snapshot(&self, s: &Snapshot) {
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
                                    &profile_page(s, p));
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

fn profile_page(s: &Snapshot, p: &ProfileSector) -> adw::PreferencesPage {
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
    overview.add(&info_row("State", state));
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

            let dpi = group("DPI");
            for (slot, &v) in prof.dpi.iter().enumerate() {
                let mut tags = Vec::new();
                if slot == prof.default_dpi_slot as usize {
                    tags.push("default");
                }
                if slot == prof.shift_dpi_slot as usize {
                    tags.push("shift");
                }
                let value = if dpi_used(v) { v.to_string() } else { "Unused".into() };
                let value = if tags.is_empty() { value } else { format!("{value}  ({})", tags.join(", ")) };
                dpi.add(&info_row(&format!("Slot {slot}"), &value));
            }
            page.add(&dpi);

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
