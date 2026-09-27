use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    App,
};

use crate::WindowLabel;

pub const REAUTHORIZE_PERMISSIONS_ARGUMENT: &str = "--reauthorize-permissions";

pub fn permission_reauthorization_requested<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .any(|arg| arg.as_ref() == REAUTHORIZE_PERMISSIONS_ARGUMENT)
}

/// What one tray menu item does when the user picks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrayMenuAction {
    Settings,
    Quit,
}

/// One tray menu item: the id Tauri hands the click back on, the label the user
/// reads, and what picking it does. The three travel together, so an item cannot
/// exist without a way to act on it.
struct TrayMenuItem {
    id: &'static str,
    label: &'static str,
    action: TrayMenuAction,
}

/// The whole tray menu, in the order it is shown.
///
/// The ids are namespaced. Unprefixed they read as window labels, and the tray
/// "Settings…" item was called "settings" because the two were never told apart.
const TRAY_MENU: [TrayMenuItem; 2] = [
    TrayMenuItem {
        id: "tray.settings",
        label: "Settings\u{2026}",
        action: TrayMenuAction::Settings,
    },
    TrayMenuItem {
        id: "tray.quit",
        label: "Quit Slugtale",
        action: TrayMenuAction::Quit,
    },
];

/// The action the clicked item carries. A click on an id that is not on the menu
/// is not a thing that can happen, so `None` only ever means the platform sent
/// something Slugtale did not put there.
fn tray_menu_action(id: &str) -> Option<TrayMenuAction> {
    TRAY_MENU
        .iter()
        .find(|item| item.id == id)
        .map(|item| item.action)
}

pub fn show_settings(app: tauri::AppHandle) {
    if let Some(window) = WindowLabel::Settings.window(&app) {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn setup_tray(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let mut menu_items: Vec<Box<dyn tauri::menu::IsMenuItem<tauri::Wry>>> = Vec::new();
    for item in &TRAY_MENU {
        let menu_item = MenuItem::with_id(app, item.id, item.label, true, None::<&str>)?;
        menu_items.push(Box::new(menu_item));
    }

    let menu_refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> =
        menu_items.iter().map(|i| i.as_ref()).collect();
    let menu = Menu::with_items(app, &menu_refs)?;

    let icon = Image::from_bytes(include_bytes!("../icons/icon.png"))?;

    TrayIconBuilder::new()
        .icon(icon)
        .icon_as_template(true)
        .tooltip("Slugtale")
        .menu(&menu)
        .on_menu_event(|app, event| {
            let Some(action) = tray_menu_action(event.id.as_ref()) else {
                return;
            };
            match action {
                TrayMenuAction::Settings => {
                    show_settings(app.clone());
                }
                TrayMenuAction::Quit => {
                    app.exit(0);
                }
            }
        })
        .build(app)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_app_reauthorization_mode_is_selected_by_its_launch_argument() {
        assert!(permission_reauthorization_requested([
            "/Applications/Slugtale.app/Contents/MacOS/slugtale",
            "--reauthorize-permissions",
        ]));
        assert!(!permission_reauthorization_requested([
            "/Applications/Slugtale.app/Contents/MacOS/slugtale",
        ]));
    }

    #[test]
    fn the_reauthorize_argument_is_matched_exactly_and_in_any_position() {
        // macOS passes the arguments through in whatever order it was launched,
        // and the flag is one string among the path and any others the user typed.
        assert!(permission_reauthorization_requested([
            "--verbose",
            "/Applications/Slugtale.app/Contents/MacOS/slugtale",
            "--reauthorize-permissions",
        ]));
        // A longer argument that merely starts with the flag is a different
        // argument, and re-authorizing on it would reset permissions the user
        // never asked to reset.
        assert!(!permission_reauthorization_requested([
            "/Applications/Slugtale.app/Contents/MacOS/slugtale",
            "--reauthorize-permissions-please",
        ]));
    }

    #[test]
    fn the_tray_menu_reads_the_way_the_specification_says() {
        let labels: Vec<&str> = TRAY_MENU.iter().map(|item| item.label).collect();
        assert_eq!(labels, ["Settings\u{2026}", "Quit Slugtale"]);
    }

    #[test]
    fn every_tray_item_is_reachable_and_does_what_it_carries() {
        for item in &TRAY_MENU {
            assert_eq!(tray_menu_action(item.id), Some(item.action));
        }
    }

    #[test]
    fn a_menu_id_that_is_not_on_the_menu_is_not_an_action() {
        assert_eq!(tray_menu_action("about"), None);
    }

    #[test]
    fn tray_menu_ids_are_not_window_labels() {
        // The two namespaces are separate strings that happen to live in the same
        // process. Nothing turns a tray id into a window, and nothing should read
        // as though it might.
        for item in &TRAY_MENU {
            assert_eq!(WindowLabel::from_label(item.id), None);
        }
    }
}
