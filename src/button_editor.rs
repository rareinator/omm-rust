//! Dialog for choosing the action of one button entry.

use adw::prelude::*;
use gtk::glib;

use crate::profile::{self, Action, CONSUMER_KEYS, MODIFIER_NAMES};

const KINDS: [&str; 5] = ["Default (not remapped)", "Mouse button", "Keyboard key", "Media key", "Built-in function"];
const KIND_KEEP: u32 = KINDS.len() as u32;
const MOUSE_SLOTS: usize = 5;

fn combo_row(title: &str, items: &[String], searchable: bool) -> adw::ComboRow {
    let strs: Vec<&str> = items.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&strs))
        .use_subtitle(false)
        .build();
    if searchable {
        row.set_expression(Some(&gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<&gtk::Expression>,
            "string",
        )));
        row.set_enable_search(true);
    }
    row
}

fn position<T: PartialEq>(items: &[T], want: &T) -> u32 {
    items.iter().position(|x| x == want).unwrap_or(0) as u32
}

/// Ask for a new action for `current`; `on_set` gets the encoded entry. Entries the
/// editor can't represent (macros, combined buttons, ...) get a "Keep current" choice.
pub fn edit_action(parent: &gtk::Widget, title: &str, current: [u8; 4], on_set: impl Fn([u8; 4]) + 'static) {
    let action = Action::decode(&current);

    let mut kinds: Vec<String> = KINDS.iter().map(|s| s.to_string()).collect();
    if matches!(action, Action::Other(_)) {
        kinds.push(format!("Keep current: {}", profile::decode_button(&current)));
    }
    let kind_row = combo_row("Action", &kinds, false);

    let mouse_names: Vec<String> = (0..MOUSE_SLOTS).map(profile::slot_name).collect();
    let mouse_row = combo_row("Button", &mouse_names, false);

    let keys = profile::named_keys();
    let key_names: Vec<String> = keys.iter().map(|(_, n)| n.clone()).collect();
    let key_row = combo_row("Key", &key_names, true);
    let mod_checks: Vec<gtk::CheckButton> = MODIFIER_NAMES.iter().map(|n| gtk::CheckButton::with_label(n)).collect();
    let mods_box = gtk::Box::builder().spacing(6).valign(gtk::Align::Center).build();
    mod_checks.iter().for_each(|c| mods_box.append(c));
    let mods_row = adw::ActionRow::builder().title("Modifiers").build();
    mods_row.add_suffix(&mods_box);

    let media_names: Vec<String> = CONSUMER_KEYS.iter().map(|(_, n)| n.to_string()).collect();
    let media_row = combo_row("Media key", &media_names, false);

    let functions = profile::button_functions();
    let function_names: Vec<String> = functions.iter().map(|(_, n)| n.to_string()).collect();
    let function_row = combo_row("Function", &function_names, true);

    // Pre-select the current action
    match action {
        Action::Default => kind_row.set_selected(0),
        Action::Mouse(bit) => {
            kind_row.set_selected(1);
            mouse_row.set_selected(bit as u32);
        }
        Action::Key { mods, key } => {
            kind_row.set_selected(2);
            key_row.set_selected(position(&keys.iter().map(|(c, _)| *c).collect::<Vec<_>>(), &key));
            for (i, c) in mod_checks.iter().enumerate() {
                c.set_active(mods & (1 << i) != 0);
            }
        }
        Action::Consumer(code) => {
            kind_row.set_selected(3);
            media_row.set_selected(position(&CONSUMER_KEYS.iter().map(|(c, _)| *c).collect::<Vec<_>>(), &code));
        }
        Action::Function(id) => {
            kind_row.set_selected(4);
            function_row.set_selected(position(&functions.iter().map(|(i, _)| *i).collect::<Vec<_>>(), &id));
        }
        Action::Other(_) => kind_row.set_selected(KIND_KEEP),
    }

    let list = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
    let detail_rows: [(u32, gtk::Widget); 5] = [
        (1, mouse_row.clone().upcast()),
        (2, key_row.clone().upcast()),
        (2, mods_row.clone().upcast()),
        (3, media_row.clone().upcast()),
        (4, function_row.clone().upcast()),
    ];
    list.append(&kind_row);
    for (_, w) in &detail_rows {
        list.append(w);
    }
    let show_kind = move |kind: u32| {
        for (k, w) in &detail_rows {
            w.set_visible(*k == kind);
        }
    };
    show_kind(kind_row.selected());
    kind_row.connect_selected_notify(move |row| show_kind(row.selected()));

    let dialog = adw::AlertDialog::new(Some(title), Some("Takes effect when you apply the button changes."));
    dialog.set_extra_child(Some(&list));
    dialog.add_responses(&[("cancel", "Cancel"), ("set", "Set")]);
    dialog.set_response_appearance("set", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("set"));
    dialog.set_close_response("cancel");
    dialog.connect_response(Some("set"), glib::clone!(
        #[weak]
        kind_row,
        move |_, _| {
            let new = match kind_row.selected() {
                0 => Action::Default,
                1 => Action::Mouse(mouse_row.selected() as usize),
                2 => {
                    let mods = mod_checks
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.is_active())
                        .fold(0u8, |m, (i, _)| m | (1 << i));
                    Action::Key { mods, key: keys[key_row.selected() as usize].0 }
                }
                3 => Action::Consumer(CONSUMER_KEYS[media_row.selected() as usize].0),
                4 => Action::Function(functions[function_row.selected() as usize].0),
                _ => Action::Other(current),
            };
            on_set(new.encode());
        }
    ));
    dialog.present(Some(parent));
}
